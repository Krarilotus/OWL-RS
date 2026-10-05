//! Sets of data values: the Boolean algebra the datatype theory decides data ranges in
//! (owl2-dl.md, "The datatype theory"). Every datatype, facet restriction, enumeration and
//! their unions, intersections and complements (within `rdfs:Literal`, every value of the
//! map) is a `ValueSet`, exactly, except where [`facet`] says it can't be (patterns,
//! language ranges, the length of language-tagged strings).
//!
//! Per value space, a form that complements and counts exactly:
//! - **numbers of `owl:real`:** one interval line per class of the partition integers,
//!   decimals that aren't integers, rationals that aren't decimals, irrationals, so that
//!   every numeric datatype is a union of classes and every facet an interval;
//! - **floats and doubles:** intervals of their ordinals (the values in order, `-0` just
//!   before `+0`), and NaN apart;
//! - **strings:** lengths per region of the chain of string types (`text`);
//! - **date-times:** the instants of those with a timezone, the local times of those
//!   without (each with the comparison XML Schema defines between the two);
//! - **binary data, IRIs:** lengths;
//! - and, over all of these, finitely many values added or taken out.
//!
//! Counts are bounds (`Count`): exact wherever a set is small, a lower bound of a
//! thousand or more where strings are many.

use std::collections::BTreeSet;
use std::ops::Bound;

use super::datatype::{Datatype, Facet};
use super::line::{Line, integer_range};
use super::rational::Rational;
use super::text;
use super::value::Value;

/// How many values a set has: at least `lo`, at most `hi` (`u64::MAX`: unbounded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Count {
    pub lo: u64,
    pub hi: u64,
}

impl Count {
    pub const ZERO: Self = Self { lo: 0, hi: 0 };
    pub const MANY: Self = Self {
        lo: u64::MAX,
        hi: u64::MAX,
    };

    pub fn exact(n: u64) -> Self {
        Self { lo: n, hi: n }
    }

    pub fn is_empty(self) -> bool {
        self.hi == 0
    }

    /// At least `n` values, for certain.
    pub fn at_least(self, n: u64) -> bool {
        self.lo >= n
    }

    /// Fewer than `n` values, for certain.
    pub fn below(self, n: u64) -> bool {
        self.hi < n
    }

    fn plus(self, other: Self) -> Self {
        Self {
            lo: self.lo.saturating_add(other.lo),
            hi: self.hi.saturating_add(other.hi),
        }
    }

    fn minus(self, n: u64) -> Self {
        Self {
            lo: self.lo.saturating_sub(n),
            hi: if self.hi == u64::MAX {
                u64::MAX
            } else {
                self.hi.saturating_sub(n)
            },
        }
    }
}

// Families with a base form and finitely many exceptions ---------------------------------

/// A form of a value space that complements, intersects and counts.
trait Base: Clone + PartialEq + std::fmt::Debug {
    type V: Ord + Clone + std::fmt::Debug;
    fn contains(&self, v: &Self::V) -> bool;
    fn union(&self, other: &Self) -> Self;
    fn intersection(&self, other: &Self) -> Self;
    fn complement(&self) -> Self;
    fn count(&self) -> Count;
    /// Every value, if they are at most `limit` and known.
    fn values(&self, limit: u64) -> Option<Vec<Self::V>>;
}

/// A base form with values added (`plus`, outside the base) and taken out (`minus`,
/// inside it).
#[derive(Debug, Clone, PartialEq)]
struct Patched<B: Base> {
    base: B,
    plus: BTreeSet<B::V>,
    minus: BTreeSet<B::V>,
}

impl<B: Base> Patched<B> {
    fn of(base: B) -> Self {
        Self {
            base,
            plus: BTreeSet::new(),
            minus: BTreeSet::new(),
        }
    }

    fn contains(&self, v: &B::V) -> bool {
        self.plus.contains(v) || (self.base.contains(v) && !self.minus.contains(v))
    }

    fn complement(&self) -> Self {
        Self {
            base: self.base.complement(),
            plus: self.minus.clone(),
            minus: self.plus.clone(),
        }
    }

    fn combine(&self, other: &Self, base: B, op: impl Fn(bool, bool) -> bool) -> Self {
        let mut out = Self::of(base);
        let explicit = self
            .plus
            .iter()
            .chain(&self.minus)
            .chain(&other.plus)
            .chain(&other.minus);
        for v in explicit {
            let member = op(self.contains(v), other.contains(v));
            let covered = out.base.contains(v);
            if member && !covered {
                out.plus.insert(v.clone());
            } else if !member && covered {
                out.minus.insert(v.clone());
            }
        }
        out
    }

    fn union(&self, other: &Self) -> Self {
        self.combine(other, self.base.union(&other.base), |a, b| a || b)
    }

    fn intersection(&self, other: &Self) -> Self {
        self.combine(other, self.base.intersection(&other.base), |a, b| a && b)
    }

    fn count(&self) -> Count {
        self.base
            .count()
            .minus(self.minus.len() as u64)
            .plus(Count::exact(self.plus.len() as u64))
    }

    fn values(&self, limit: u64) -> Option<Vec<B::V>> {
        let mut out: Vec<B::V> = self
            .base
            .values(limit.saturating_add(self.minus.len() as u64))?
            .into_iter()
            .filter(|v| !self.minus.contains(v))
            .collect();
        out.extend(self.plus.iter().cloned());
        (out.len() as u64 <= limit).then_some(out)
    }
}

/// Values summed over the lengths (integer points from `floor`) of a line, `per` giving
/// each length's count. Every family counted so has at least a thousand values of each
/// length past 64, so a longer run is "many" past its first 64 lengths.
fn count_integers(line: &Line<i128>, floor: i128, per: impl Fn(i128) -> Count) -> Count {
    let mut total = Count::ZERO;
    for s in line.segments() {
        let (lo, hi) = integer_range(&s);
        let lo = lo.unwrap_or(floor).max(floor);
        let hi = hi.unwrap_or(i128::MAX);
        if lo > hi {
            continue;
        }
        let last = hi.min(lo.saturating_add(63));
        for p in lo..=last {
            total = total.plus(per(p));
        }
        if last < hi {
            total.hi = u64::MAX;
        }
    }
    total
}

/// The integer points of a line (floating-point ordinals).
fn count_ordinals(line: &Line<i128>) -> Count {
    line.segments().iter().fold(Count::ZERO, |n, s| {
        n.plus(match integer_range(s) {
            (Some(lo), Some(hi)) if lo > hi => Count::ZERO,
            (Some(lo), Some(hi)) => Count::exact(u64::try_from(hi - lo + 1).unwrap_or(u64::MAX)),
            _ => Count::MANY,
        })
    })
}

/// Strings, by length per region.
#[derive(Debug, Clone, PartialEq)]
struct Strings([Line<i128>; text::REGIONS]);

impl Strings {
    fn none() -> Self {
        Self(std::array::from_fn(|_| Line::empty()))
    }

    /// The regions from `floor` on, any length.
    fn from_region(floor: usize) -> Self {
        Self(std::array::from_fn(|r| {
            if r >= floor {
                Line::full()
            } else {
                Line::empty()
            }
        }))
    }

    fn map(&self, f: impl Fn(&Line<i128>) -> Line<i128>) -> Self {
        Self(std::array::from_fn(|r| f(&self.0[r])))
    }

    fn zip(&self, o: &Self, f: impl Fn(&Line<i128>, &Line<i128>) -> Line<i128>) -> Self {
        Self(std::array::from_fn(|r| f(&self.0[r], &o.0[r])))
    }
}

fn char_length(s: &str) -> i128 {
    s.chars().count() as i128
}

impl Base for Strings {
    type V = String;

    fn contains(&self, v: &String) -> bool {
        self.0[text::region(v)].contains(&char_length(v))
    }

    fn union(&self, o: &Self) -> Self {
        self.zip(o, Line::union)
    }

    fn intersection(&self, o: &Self) -> Self {
        self.zip(o, Line::intersection)
    }

    fn complement(&self) -> Self {
        self.map(Line::complement)
    }

    fn count(&self) -> Count {
        let mut total = Count::ZERO;
        for (r, line) in self.0.iter().enumerate() {
            total = total.plus(count_integers(line, 0, |len| {
                let (lo, hi) = text::count(r, len as u64);
                Count { lo, hi }
            }));
        }
        total
    }

    fn values(&self, limit: u64) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for (r, line) in self.0.iter().enumerate() {
            for s in line.segments() {
                let (lo, hi) = integer_range(&s);
                let lo = lo.unwrap_or(0).max(0);
                let hi = hi?;
                for len in lo..=hi {
                    out.extend(text::strings(r, len as u64)?);
                    if out.len() as u64 > limit {
                        return None;
                    }
                }
            }
        }
        Some(out)
    }
}

/// Sequences (of bytes, or characters of an IRI) by length.
#[derive(Debug, Clone, PartialEq)]
struct Lengths<V> {
    line: Line<i128>,
    marker: std::marker::PhantomData<V>,
}

impl<V> Lengths<V> {
    fn of(line: Line<i128>) -> Self {
        Self {
            line,
            marker: std::marker::PhantomData,
        }
    }
}

/// What a sequence's length is and how many there are of a length.
trait Sequence: Ord + Clone + std::fmt::Debug + PartialEq {
    fn length(&self) -> i128;
    fn count(length: i128) -> Count;
    fn all(length: i128, limit: u64) -> Option<Vec<Self>>;
}

impl Sequence for Vec<u8> {
    fn length(&self) -> i128 {
        self.len() as i128
    }

    fn count(length: i128) -> Count {
        // 256^length.
        let n = u32::try_from(length)
            .ok()
            .and_then(|l| 256u64.checked_pow(l))
            .unwrap_or(u64::MAX);
        Count::exact(n)
    }

    fn all(length: i128, limit: u64) -> Option<Vec<Self>> {
        if Self::count(length).hi > limit {
            return None;
        }
        let mut out = vec![Vec::new()];
        for _ in 0..length {
            out = out
                .into_iter()
                .flat_map(|v| {
                    (0..=255u8).map(move |b| {
                        let mut w = v.clone();
                        w.push(b);
                        w
                    })
                })
                .collect();
        }
        Some(out)
    }
}

impl Sequence for String {
    fn length(&self) -> i128 {
        char_length(self)
    }

    fn count(length: i128) -> Count {
        if length == 0 {
            Count::exact(1)
        } else {
            Count {
                lo: 1000,
                hi: u64::MAX,
            }
        }
    }

    fn all(length: i128, _: u64) -> Option<Vec<Self>> {
        (length == 0).then(|| vec![String::new()])
    }
}

impl<V: Sequence> Base for Lengths<V> {
    type V = V;

    fn contains(&self, v: &V) -> bool {
        self.line.contains(&v.length())
    }

    fn union(&self, o: &Self) -> Self {
        Self::of(self.line.union(&o.line))
    }

    fn intersection(&self, o: &Self) -> Self {
        Self::of(self.line.intersection(&o.line))
    }

    fn complement(&self) -> Self {
        Self::of(self.line.complement())
    }

    fn count(&self) -> Count {
        count_integers(&self.line, 0, V::count)
    }

    fn values(&self, limit: u64) -> Option<Vec<V>> {
        let mut out = Vec::new();
        for s in self.line.segments() {
            let (lo, hi) = integer_range(&s);
            for len in lo.unwrap_or(0).max(0)..=hi? {
                out.extend(V::all(len, limit)?);
                if out.len() as u64 > limit {
                    return None;
                }
            }
        }
        Some(out)
    }
}

/// All or nothing of an infinite value space without facets.
#[derive(Debug, Clone, PartialEq)]
struct Whole<V> {
    all: bool,
    marker: std::marker::PhantomData<V>,
}

impl<V> Whole<V> {
    fn of(all: bool) -> Self {
        Self {
            all,
            marker: std::marker::PhantomData,
        }
    }
}

impl<V: Ord + Clone + std::fmt::Debug + PartialEq> Base for Whole<V> {
    type V = V;

    fn contains(&self, _: &V) -> bool {
        self.all
    }

    fn union(&self, o: &Self) -> Self {
        Self::of(self.all || o.all)
    }

    fn intersection(&self, o: &Self) -> Self {
        Self::of(self.all && o.all)
    }

    fn complement(&self) -> Self {
        Self::of(!self.all)
    }

    fn count(&self) -> Count {
        if self.all { Count::MANY } else { Count::ZERO }
    }

    fn values(&self, _: u64) -> Option<Vec<V>> {
        (!self.all).then(Vec::new)
    }
}

/// The instants of date-times with a timezone (each instant 1,681 values, one per
/// offset from −14:00 to +14:00) and the local times of those without.
#[derive(Debug, Clone, PartialEq)]
struct Times {
    zoned: Line<i128>,
    local: Line<i128>,
}

/// Fourteen hours in the instants' unit (seconds × 10¹⁸).
const FOURTEEN_HOURS: i128 = 14 * 3600 * 1_000_000_000_000_000_000;
const OFFSETS: u64 = 2 * 14 * 60 + 1;

impl Base for Times {
    type V = (i128, Option<i16>);

    fn contains(&self, v: &Self::V) -> bool {
        match v.1 {
            Some(_) => self.zoned.contains(&v.0),
            None => self.local.contains(&v.0),
        }
    }

    fn union(&self, o: &Self) -> Self {
        Self {
            zoned: self.zoned.union(&o.zoned),
            local: self.local.union(&o.local),
        }
    }

    fn intersection(&self, o: &Self) -> Self {
        Self {
            zoned: self.zoned.intersection(&o.zoned),
            local: self.local.intersection(&o.local),
        }
    }

    fn complement(&self) -> Self {
        Self {
            zoned: self.zoned.complement(),
            local: self.local.complement(),
        }
    }

    fn count(&self) -> Count {
        // A dense line: an interval of more than a point has infinitely many values.
        let dense = |line: &Line<i128>, per_point: u64| {
            line.segments().iter().fold(Count::ZERO, |n, s| {
                n.plus(if s.point().is_some() {
                    Count::exact(per_point)
                } else {
                    Count::MANY
                })
            })
        };
        dense(&self.zoned, OFFSETS).plus(dense(&self.local, 1))
    }

    fn values(&self, limit: u64) -> Option<Vec<Self::V>> {
        let mut out = Vec::new();
        for s in self.zoned.segments() {
            let t = *s.point()?;
            out.extend((-840..=840).map(|m| (t, Some(m))));
        }
        for s in self.local.segments() {
            out.push((*s.point()?, None));
        }
        (out.len() as u64 <= limit).then_some(out)
    }
}

// Numbers --------------------------------------------------------------------------------

/// The classes of `owl:real`: integers, decimals that aren't integers, rationals that
/// aren't decimals, irrationals.
const CLASSES: usize = 4;

fn class_of(r: &Rational) -> usize {
    if r.is_integer() {
        0
    } else if r.is_decimal() {
        1
    } else {
        2
    }
}

/// The values of a class within an interval line.
fn count_class(class: usize, line: &Line<Rational>) -> Count {
    let mut total = Count::ZERO;
    for s in line.segments() {
        if class == 0 {
            let lo = s.from.map(|c| {
                if c.after {
                    c.at.floor() + 1
                } else {
                    c.at.ceil()
                }
            });
            let hi = s.to.map(|c| {
                if c.after {
                    c.at.floor()
                } else {
                    c.at.ceil() - 1
                }
            });
            match (lo, hi) {
                (Some(lo), Some(hi)) if lo > hi => {}
                (Some(lo), Some(hi)) => {
                    let n = u64::try_from(hi - lo + 1).unwrap_or(u64::MAX);
                    total = total.plus(Count::exact(n));
                }
                _ => return Count::MANY,
            }
        } else {
            match s.point() {
                Some(p) => {
                    if class_of(p) == class {
                        total = total.plus(Count::exact(1));
                    }
                }
                None => return Count::MANY,
            }
        }
    }
    total
}

/// Floating-point values by ordinal: their order, `-0` just before `+0`.
pub(crate) fn ordinal32(bits: u32) -> i128 {
    if bits >> 31 == 0 {
        i128::from(bits)
    } else {
        -i128::from(bits & 0x7fff_ffff) - 1
    }
}

pub(crate) fn ordinal64(bits: u64) -> i128 {
    if bits >> 63 == 0 {
        i128::from(bits)
    } else {
        -i128::from(bits & 0x7fff_ffff_ffff_ffff) - 1
    }
}

fn bits32(ordinal: i128) -> u32 {
    if ordinal >= 0 {
        ordinal as u32
    } else {
        ((-ordinal - 1) as u32) | 0x8000_0000
    }
}

fn bits64(ordinal: i128) -> u64 {
    if ordinal >= 0 {
        ordinal as u64
    } else {
        ((-ordinal - 1) as u64) | 0x8000_0000_0000_0000
    }
}

const INF32: i128 = 0x7f80_0000;
const INF64: i128 = 0x7ff0_0000_0000_0000;

/// Every float, or double, but NaN.
fn floats(inf: i128) -> Line<i128> {
    Line::interval(Bound::Included(-inf - 1), Bound::Included(inf))
}

// The set ---------------------------------------------------------------------------------

/// A set of data values.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueSet {
    real: [Line<Rational>; CLASSES],
    float: Line<i128>,
    float_nan: bool,
    double: Line<i128>,
    double_nan: bool,
    strings: Patched<Strings>,
    tagged: Patched<Whole<(String, String)>>,
    boolean: [bool; 2],
    times: Patched<Times>,
    hex: Patched<Lengths<Vec<u8>>>,
    base64: Patched<Lengths<Vec<u8>>>,
    iris: Patched<Lengths<String>>,
    xml: Patched<Whole<String>>,
}

impl ValueSet {
    pub fn empty() -> Self {
        Self {
            real: std::array::from_fn(|_| Line::empty()),
            float: Line::empty(),
            float_nan: false,
            double: Line::empty(),
            double_nan: false,
            strings: Patched::of(Strings::none()),
            tagged: Patched::of(Whole::of(false)),
            boolean: [false; 2],
            times: Patched::of(Times {
                zoned: Line::empty(),
                local: Line::empty(),
            }),
            hex: Patched::of(Lengths::of(Line::empty())),
            base64: Patched::of(Lengths::of(Line::empty())),
            iris: Patched::of(Lengths::of(Line::empty())),
            xml: Patched::of(Whole::of(false)),
        }
    }

    /// `rdfs:Literal`: every value.
    pub fn all() -> Self {
        Self::empty().complement()
    }

    /// The values of a datatype.
    pub fn of(datatype: Datatype) -> Self {
        let mut s = Self::empty();
        match datatype {
            Datatype::Literal => return Self::all(),
            Datatype::Real => s.real = std::array::from_fn(|_| Line::full()),
            Datatype::Rational => {
                s.real = std::array::from_fn(|c| if c < 3 { Line::full() } else { Line::empty() })
            }
            Datatype::Decimal => {
                s.real = std::array::from_fn(|c| if c < 2 { Line::full() } else { Line::empty() })
            }
            Datatype::Float => {
                s.float = floats(INF32);
                s.float_nan = true;
            }
            Datatype::Double => {
                s.double = floats(INF64);
                s.double_nan = true;
            }
            Datatype::PlainLiteral => {
                s.strings = Patched::of(Strings::from_region(0));
                s.tagged = Patched::of(Whole::of(true));
            }
            Datatype::LangString => s.tagged = Patched::of(Whole::of(true)),
            Datatype::Boolean => s.boolean = [true; 2],
            Datatype::HexBinary => s.hex = Patched::of(Lengths::of(non_negative())),
            Datatype::Base64Binary => s.base64 = Patched::of(Lengths::of(non_negative())),
            Datatype::AnyUri => s.iris = Patched::of(Lengths::of(non_negative())),
            Datatype::DateTime => {
                s.times = Patched::of(Times {
                    zoned: Line::full(),
                    local: Line::full(),
                })
            }
            Datatype::DateTimeStamp => {
                s.times = Patched::of(Times {
                    zoned: Line::full(),
                    local: Line::empty(),
                })
            }
            Datatype::XmlLiteral => s.xml = Patched::of(Whole::of(true)),
            d => {
                if let Some((lo, hi)) = d.integer_bounds() {
                    let bound = |b: Option<i128>| {
                        b.and_then(Rational::integer)
                            .map_or(Bound::Unbounded, Bound::Included)
                    };
                    s.real[0] = Line::interval(bound(lo), bound(hi));
                } else if let Some(floor) = d.string_floor() {
                    s.strings = Patched::of(Strings::from_region(floor));
                }
            }
        }
        s
    }

    /// `{value}`.
    pub fn single(value: &Value) -> Self {
        let mut s = Self::empty();
        match value {
            Value::Real(r) => s.real[class_of(r)] = Line::point(*r),
            Value::Float(b) => {
                if f32::from_bits(*b).is_nan() {
                    s.float_nan = true;
                } else {
                    s.float = Line::point(ordinal32(*b));
                }
            }
            Value::Double(b) => {
                if f64::from_bits(*b).is_nan() {
                    s.double_nan = true;
                } else {
                    s.double = Line::point(ordinal64(*b));
                }
            }
            Value::String(v) => {
                s.strings.plus.insert(v.clone());
            }
            Value::LangString(v, tag) => {
                s.tagged.plus.insert((v.clone(), tag.clone()));
            }
            Value::Boolean(b) => s.boolean[usize::from(*b)] = true,
            Value::DateTime(t, tz) => {
                s.times.plus.insert((*t, *tz));
            }
            Value::HexBinary(v) => {
                s.hex.plus.insert(v.clone());
            }
            Value::Base64Binary(v) => {
                s.base64.plus.insert(v.clone());
            }
            Value::AnyUri(v) => {
                s.iris.plus.insert(v.clone());
            }
            Value::XmlLiteral(v) => {
                s.xml.plus.insert(v.clone());
            }
        }
        s
    }

    pub fn contains(&self, value: &Value) -> bool {
        match value {
            Value::Real(r) => self.real[class_of(r)].contains(r),
            Value::Float(b) => {
                if f32::from_bits(*b).is_nan() {
                    self.float_nan
                } else {
                    self.float.contains(&ordinal32(*b))
                }
            }
            Value::Double(b) => {
                if f64::from_bits(*b).is_nan() {
                    self.double_nan
                } else {
                    self.double.contains(&ordinal64(*b))
                }
            }
            Value::String(v) => self.strings.contains(v),
            Value::LangString(v, tag) => self.tagged.contains(&(v.clone(), tag.clone())),
            Value::Boolean(b) => self.boolean[usize::from(*b)],
            Value::DateTime(t, tz) => self.times.contains(&(*t, *tz)),
            Value::HexBinary(v) => self.hex.contains(v),
            Value::Base64Binary(v) => self.base64.contains(v),
            Value::AnyUri(v) => self.iris.contains(v),
            Value::XmlLiteral(v) => self.xml.contains(v),
        }
    }

    /// Every value not in it (within `rdfs:Literal`).
    pub fn complement(&self) -> Self {
        let lengths = |p: &Patched<Lengths<Vec<u8>>>| {
            let c = p.complement();
            Patched {
                base: Lengths::of(c.base.line.intersection(&non_negative())),
                ..c
            }
        };
        let iris = self.iris.complement();
        Self {
            real: std::array::from_fn(|c| self.real[c].complement()),
            float: self.float.complement().intersection(&floats(INF32)),
            float_nan: !self.float_nan,
            double: self.double.complement().intersection(&floats(INF64)),
            double_nan: !self.double_nan,
            strings: self.strings.complement(),
            tagged: self.tagged.complement(),
            boolean: [!self.boolean[0], !self.boolean[1]],
            times: self.times.complement(),
            hex: lengths(&self.hex),
            base64: lengths(&self.base64),
            iris: Patched {
                base: Lengths::of(iris.base.line.intersection(&non_negative())),
                ..iris
            },
            xml: self.xml.complement(),
        }
    }

    pub fn union(&self, o: &Self) -> Self {
        Self {
            real: std::array::from_fn(|c| self.real[c].union(&o.real[c])),
            float: self.float.union(&o.float),
            float_nan: self.float_nan || o.float_nan,
            double: self.double.union(&o.double),
            double_nan: self.double_nan || o.double_nan,
            strings: self.strings.union(&o.strings),
            tagged: self.tagged.union(&o.tagged),
            boolean: [
                self.boolean[0] || o.boolean[0],
                self.boolean[1] || o.boolean[1],
            ],
            times: self.times.union(&o.times),
            hex: self.hex.union(&o.hex),
            base64: self.base64.union(&o.base64),
            iris: self.iris.union(&o.iris),
            xml: self.xml.union(&o.xml),
        }
    }

    pub fn intersection(&self, o: &Self) -> Self {
        Self {
            real: std::array::from_fn(|c| self.real[c].intersection(&o.real[c])),
            float: self.float.intersection(&o.float),
            float_nan: self.float_nan && o.float_nan,
            double: self.double.intersection(&o.double),
            double_nan: self.double_nan && o.double_nan,
            strings: self.strings.intersection(&o.strings),
            tagged: self.tagged.intersection(&o.tagged),
            boolean: [
                self.boolean[0] && o.boolean[0],
                self.boolean[1] && o.boolean[1],
            ],
            times: self.times.intersection(&o.times),
            hex: self.hex.intersection(&o.hex),
            base64: self.base64.intersection(&o.base64),
            iris: self.iris.intersection(&o.iris),
            xml: self.xml.intersection(&o.xml),
        }
    }

    /// How many values it has.
    pub fn count(&self) -> Count {
        let mut n = Count::ZERO;
        for (c, line) in self.real.iter().enumerate() {
            n = n.plus(count_class(c, line));
        }
        n = n.plus(count_ordinals(&self.float));
        n = n.plus(count_ordinals(&self.double));
        n = n.plus(Count::exact(
            u64::from(self.float_nan) + u64::from(self.double_nan),
        ));
        n = n.plus(Count::exact(
            self.boolean.iter().filter(|&&b| b).count() as u64
        ));
        n.plus(self.strings.count())
            .plus(self.tagged.count())
            .plus(self.times.count())
            .plus(self.hex.count())
            .plus(self.base64.count())
            .plus(self.iris.count())
            .plus(self.xml.count())
    }

    pub fn is_empty(&self) -> bool {
        self.count().is_empty()
    }

    /// Every value, if there are at most `limit` and each is known (strings of the large
    /// regions aren't listed).
    pub fn values(&self, limit: u64) -> Option<Vec<Value>> {
        if self.count().hi > limit {
            return None;
        }
        let mut out = Vec::new();
        for (c, line) in self.real.iter().enumerate() {
            for s in line.segments() {
                if let Some(p) = s.point() {
                    out.push(Value::Real(*p));
                    continue;
                }
                if c != 0 {
                    return None;
                }
                let lo = s.from?;
                let hi = s.to?;
                let lo = if lo.after {
                    lo.at.floor() + 1
                } else {
                    lo.at.ceil()
                };
                let hi = if hi.after {
                    hi.at.floor()
                } else {
                    hi.at.ceil() - 1
                };
                for i in lo..=hi {
                    out.push(Value::Real(Rational::integer(i)?));
                }
            }
        }
        for (line, double) in [(&self.float, false), (&self.double, true)] {
            for s in line.segments() {
                let (lo, hi) = integer_range(&s);
                for o in lo?..=hi? {
                    out.push(if double {
                        Value::Double(bits64(o))
                    } else {
                        Value::Float(bits32(o))
                    });
                }
            }
        }
        if self.float_nan {
            out.push(Value::Float(0x7fc0_0000));
        }
        if self.double_nan {
            out.push(Value::Double(0x7ff8_0000_0000_0000));
        }
        for b in [false, true] {
            if self.boolean[usize::from(b)] {
                out.push(Value::Boolean(b));
            }
        }
        out.extend(self.strings.values(limit)?.into_iter().map(Value::String));
        out.extend(
            self.tagged
                .values(limit)?
                .into_iter()
                .map(|(v, t)| Value::LangString(v, t)),
        );
        out.extend(
            self.times
                .values(limit)?
                .into_iter()
                .map(|(t, z)| Value::DateTime(t, z)),
        );
        out.extend(self.hex.values(limit)?.into_iter().map(Value::HexBinary));
        out.extend(
            self.base64
                .values(limit)?
                .into_iter()
                .map(Value::Base64Binary),
        );
        out.extend(self.iris.values(limit)?.into_iter().map(Value::AnyUri));
        out.extend(self.xml.values(limit)?.into_iter().map(Value::XmlLiteral));
        (out.len() as u64 <= limit).then_some(out)
    }
}

fn non_negative() -> Line<i128> {
    Line::interval(Bound::Included(0), Bound::Unbounded)
}

/// The values of `datatype` that satisfy the facet with `value`, or `None` where that
/// isn't decided here (patterns, language ranges, lengths of language-tagged strings) or
/// the facet doesn't apply to the datatype with that value (not in its facet space).
pub fn facet(datatype: Datatype, facet: Facet, value: &Value) -> Option<ValueSet> {
    let mut s = ValueSet::empty();
    let order = matches!(
        facet,
        Facet::MinInclusive | Facet::MaxInclusive | Facet::MinExclusive | Facet::MaxExclusive
    );
    let bounds = |p| match facet {
        Facet::MinInclusive => (Bound::Included(p), Bound::Unbounded),
        Facet::MinExclusive => (Bound::Excluded(p), Bound::Unbounded),
        Facet::MaxInclusive => (Bound::Unbounded, Bound::Included(p)),
        _ => (Bound::Unbounded, Bound::Excluded(p)),
    };
    match (datatype, value) {
        (d, Value::Real(r)) if order && d.is_real() => {
            let (lo, hi) = bounds(*r);
            let line = Line::interval(lo, hi);
            s.real = std::array::from_fn(|_| line.clone());
        }
        (Datatype::Float, Value::Float(b)) if order => {
            s.float = float_bound(
                facet,
                f64::from(f32::from_bits(*b)),
                |x| ordinal32((x as f32).to_bits()),
                INF32,
            )?
        }
        (Datatype::Double, Value::Double(b)) if order => {
            s.double = float_bound(facet, f64::from_bits(*b), |x| ordinal64(x.to_bits()), INF64)?
        }
        (Datatype::DateTime | Datatype::DateTimeStamp, Value::DateTime(t, tz)) if order => {
            let (zoned, local) = time_bounds(facet, *t, tz.is_some());
            s.times = Patched::of(Times { zoned, local });
        }
        (d, Value::Real(r))
            if matches!(facet, Facet::Length | Facet::MinLength | Facet::MaxLength)
                && r.is_integer()
                && r.numerator() >= 0 =>
        {
            let n = r.numerator();
            let line = match facet {
                Facet::Length => Line::point(n),
                Facet::MinLength => Line::interval(Bound::Included(n), Bound::Unbounded),
                _ => Line::interval(Bound::Included(0), Bound::Included(n)),
            };
            match d {
                Datatype::HexBinary => s.hex = Patched::of(Lengths::of(line)),
                Datatype::Base64Binary => s.base64 = Patched::of(Lengths::of(line)),
                Datatype::AnyUri => s.iris = Patched::of(Lengths::of(line)),
                d if d.string_floor().is_some() => {
                    s.strings = Patched::of(Strings(std::array::from_fn(|_| line.clone())))
                }
                // rdf:PlainLiteral's lengths count its language-tagged strings too.
                _ => return None,
            }
        }
        _ => return None,
    }
    Some(s)
}

/// The ordinals of the floats (or doubles) on one side of `x`: `≥ ±0` starts at `-0`,
/// `≤ ±0` ends at `+0` (the two zeros are equal); NaN compares with nothing.
fn float_bound(
    facet: Facet,
    x: f64,
    ordinal: impl Fn(f64) -> i128,
    inf: i128,
) -> Option<Line<i128>> {
    if x.is_nan() {
        return Some(Line::empty());
    }
    let (lo, hi) = if x == 0.0 {
        (ordinal(-0.0), ordinal(0.0))
    } else {
        (ordinal(x), ordinal(x))
    };
    let line = match facet {
        Facet::MinInclusive => Line::interval(Bound::Included(lo), Bound::Unbounded),
        Facet::MinExclusive => Line::interval(Bound::Excluded(hi), Bound::Unbounded),
        Facet::MaxInclusive => Line::interval(Bound::Unbounded, Bound::Included(hi)),
        Facet::MaxExclusive => Line::interval(Bound::Unbounded, Bound::Excluded(lo)),
        _ => return None,
    };
    Some(line.intersection(&floats(inf)))
}

/// The date-times on one side of `t` (with a timezone or not): XML Schema's partial
/// order, where a date-time without a timezone is within fourteen hours of every
/// timezone's reading of it and is neither above nor below what lies within them.
fn time_bounds(facet: Facet, t: i128, zoned: bool) -> (Line<i128>, Line<i128>) {
    let same = match facet {
        Facet::MinInclusive => Line::interval(Bound::Included(t), Bound::Unbounded),
        Facet::MinExclusive => Line::interval(Bound::Excluded(t), Bound::Unbounded),
        Facet::MaxInclusive => Line::interval(Bound::Unbounded, Bound::Included(t)),
        _ => Line::interval(Bound::Unbounded, Bound::Excluded(t)),
    };
    // The other kind is above `t` beyond fourteen hours past it, below it beyond
    // fourteen hours before it; never equal.
    let other = match facet {
        Facet::MinInclusive | Facet::MinExclusive => {
            Line::interval(Bound::Excluded(t + FOURTEEN_HOURS), Bound::Unbounded)
        }
        _ => Line::interval(Bound::Unbounded, Bound::Excluded(t - FOURTEEN_HOURS)),
    };
    if zoned { (same, other) } else { (other, same) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(lexical: &str, d: Datatype) -> Value {
        Value::parse(lexical, d, None).unwrap()
    }

    fn restricted(d: Datatype, facets: &[(Facet, Value)]) -> ValueSet {
        facets.iter().fold(ValueSet::of(d), |s, (f, v)| {
            s.intersection(&facet(d, *f, v).expect("decided"))
        })
    }

    #[test]
    fn numeric_spaces_overlap_as_the_map_says() {
        let int = ValueSet::of(Datatype::Integer);
        let byte = ValueSet::of(Datatype::Byte);
        assert_eq!(byte.count(), Count::exact(256));
        assert_eq!(
            byte.intersection(&ValueSet::of(Datatype::UnsignedInt))
                .count(),
            Count::exact(128)
        );
        assert!(
            ValueSet::of(Datatype::NonNegativeInteger)
                .intersection(&ValueSet::of(Datatype::NonPositiveInteger))
                .count()
                == Count::exact(1)
        );
        assert!(int.intersection(&ValueSet::of(Datatype::Float)).is_empty());
        assert!(
            ValueSet::of(Datatype::Float)
                .intersection(&ValueSet::of(Datatype::Double))
                .is_empty()
        );
        assert!(
            !ValueSet::of(Datatype::Rational)
                .intersection(&ValueSet::of(Datatype::Decimal).complement())
                .is_empty()
        );
        assert!(
            !ValueSet::of(Datatype::Real)
                .intersection(&ValueSet::of(Datatype::Rational).complement())
                .is_empty()
        );
        let half = lit("0.5", Datatype::Decimal);
        assert!(ValueSet::of(Datatype::Rational).contains(&half));
        assert!(!int.contains(&half));
        // integer[> 0.5, < 1.5] = {1}; decimal there is infinite.
        let f = [
            (Facet::MinExclusive, half.clone()),
            (Facet::MaxExclusive, lit("1.5", Datatype::Decimal)),
        ];
        assert_eq!(restricted(Datatype::Integer, &f).count(), Count::exact(1));
        assert_eq!(restricted(Datatype::Decimal, &f).count(), Count::MANY);
        assert_eq!(
            restricted(Datatype::Integer, &f).values(10),
            Some(vec![lit("1", Datatype::Integer)])
        );
        // The complement of {1} in integer[> 0.5, < 1.5] is empty.
        let one = ValueSet::single(&lit("1", Datatype::Integer));
        assert!(
            restricted(Datatype::Integer, &f)
                .intersection(&one.complement())
                .is_empty()
        );
        // owl:real has irrationals beside every rational.
        assert_eq!(
            restricted(
                Datatype::Real,
                &[
                    (Facet::MinInclusive, lit("1", Datatype::Integer)),
                    (Facet::MaxInclusive, lit("1", Datatype::Integer))
                ]
            )
            .count(),
            Count::exact(1)
        );
    }

    #[test]
    fn floats_are_discrete_with_two_zeros_and_nan() {
        let between = restricted(
            Datatype::Float,
            &[
                (Facet::MinExclusive, lit("0.0", Datatype::Float)),
                (
                    Facet::MaxExclusive,
                    lit("1.401298464324817e-45", Datatype::Float),
                ),
            ],
        );
        assert!(
            between.is_empty(),
            "no float between 0 and the least subnormal"
        );
        let zero = restricted(
            Datatype::Float,
            &[
                (Facet::MinInclusive, lit("0.0", Datatype::Float)),
                (Facet::MaxInclusive, lit("-0.0", Datatype::Float)),
            ],
        );
        assert_eq!(zero.count(), Count::exact(2), "-0 and +0");
        assert!(zero.contains(&lit("-0", Datatype::Float)));
        assert!(!zero.contains(&lit("NaN", Datatype::Float)));
        assert_eq!(
            ValueSet::of(Datatype::Float).count().hi,
            2 * 0x7f80_0001_u64 + 1
        );
        let not_zero = zero
            .complement()
            .intersection(&ValueSet::of(Datatype::Float));
        assert!(not_zero.contains(&lit("NaN", Datatype::Float)));
        assert!(not_zero.contains(&lit("INF", Datatype::Float)));
        assert!(!not_zero.contains(&lit("0", Datatype::Float)));
    }

    #[test]
    fn strings_by_region_and_length() {
        let token = ValueSet::of(Datatype::Token);
        let empty_string = lit("", Datatype::String);
        assert!(token.contains(&empty_string));
        let no_length = restricted(
            Datatype::String,
            &[(Facet::Length, lit("0", Datatype::Integer))],
        );
        assert_eq!(no_length.count(), Count::exact(1));
        assert_eq!(no_length.values(5), Some(vec![empty_string.clone()]));
        assert!(
            restricted(
                Datatype::Name,
                &[(Facet::MaxLength, lit("0", Datatype::Integer))]
            )
            .is_empty()
        );
        let one_letter = restricted(
            Datatype::Language,
            &[(Facet::Length, lit("1", Datatype::Integer))],
        );
        assert_eq!(one_letter.count(), Count::exact(52));
        assert!(
            ValueSet::of(Datatype::String)
                .intersection(&ValueSet::of(Datatype::AnyUri))
                .is_empty()
        );
        // A string set minus a few values.
        let s = ValueSet::single(&lit("a", Datatype::String))
            .complement()
            .intersection(&one_letter);
        assert_eq!(s.count(), Count::exact(51));
        assert!(!s.contains(&lit("a", Datatype::String)));
        assert!(s.contains(&lit("b", Datatype::String)));
        let tagged = Value::parse("a", Datatype::LangString, Some("en")).unwrap();
        assert!(ValueSet::of(Datatype::PlainLiteral).contains(&tagged));
        assert!(!ValueSet::of(Datatype::String).contains(&tagged));
        assert!(
            facet(
                Datatype::String,
                Facet::Pattern,
                &lit("a", Datatype::String)
            )
            .is_none()
        );
    }

    #[test]
    fn date_times_compare_across_timezones() {
        let z = lit("2000-01-01T12:00:00Z", Datatype::DateTime);
        let at_least = restricted(Datatype::DateTime, &[(Facet::MinInclusive, z.clone())]);
        assert!(!at_least.contains(&lit("2000-01-01T12:00:00+05:00", Datatype::DateTime)));
        assert!(at_least.contains(&lit("2000-01-01T13:00:00+01:00", Datatype::DateTime)));
        // Without a timezone: above only beyond fourteen hours.
        assert!(!at_least.contains(&lit("2000-01-02T01:00:00", Datatype::DateTime)));
        assert!(at_least.contains(&lit("2000-01-02T02:00:01", Datatype::DateTime)));
        let point = restricted(
            Datatype::DateTimeStamp,
            &[
                (Facet::MinInclusive, z.clone()),
                (Facet::MaxInclusive, z.clone()),
            ],
        );
        assert_eq!(point.count(), Count::exact(1681));
        assert_eq!(point.values(2000).map(|v| v.len()), Some(1681));
        assert!(
            ValueSet::of(Datatype::DateTimeStamp)
                .intersection(&ValueSet::single(&lit(
                    "2000-01-01T00:00:00",
                    Datatype::DateTime
                )))
                .is_empty()
        );
    }

    #[test]
    fn complement_is_within_literal_and_exact_on_members() {
        let samples = [
            lit("1", Datatype::Integer),
            lit("1/3", Datatype::Rational),
            lit("NaN", Datatype::Double),
            lit("-0", Datatype::Float),
            lit("x", Datatype::String),
            lit("true", Datatype::Boolean),
            lit("0F", Datatype::HexBinary),
            lit("urn:x", Datatype::AnyUri),
            lit("<a/>", Datatype::XmlLiteral),
            lit("2000-01-01T00:00:00", Datatype::DateTime),
        ];
        let sets = [
            ValueSet::of(Datatype::Byte),
            ValueSet::of(Datatype::Literal),
            ValueSet::single(&samples[4]),
            ValueSet::of(Datatype::HexBinary),
            restricted(
                Datatype::HexBinary,
                &[(Facet::Length, lit("1", Datatype::Integer))],
            ),
            ValueSet::of(Datatype::XmlLiteral)
                .intersection(&ValueSet::single(&samples[8]).complement()),
        ];
        for a in &sets {
            for b in &sets {
                for v in &samples {
                    assert_eq!(a.complement().contains(v), !a.contains(v), "{v:?}");
                    assert_eq!(a.union(b).contains(v), a.contains(v) || b.contains(v));
                    assert_eq!(
                        a.intersection(b).contains(v),
                        a.contains(v) && b.contains(v)
                    );
                }
            }
            assert!(a.intersection(&a.complement()).is_empty());
        }
        assert_eq!(
            restricted(
                Datatype::HexBinary,
                &[(Facet::Length, lit("1", Datatype::Integer))]
            )
            .count(),
            Count::exact(256)
        );
        assert!(ValueSet::all().count().at_least(1 << 40));
    }

    /// Random Boolean combinations of facet restrictions, enumerations and datatypes,
    /// restricted to a small universe: their counts and values are the brute-force ones.
    #[test]
    fn random_combinations_count_like_brute_force() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let int = |i: i64| lit(&i.to_string(), Datatype::Integer);
        let half = |i: i64| lit(&format!("{i}.5"), Datatype::Decimal);
        // The universe: integers -6..=6, the halves -6.5..=6.5, two strings, true.
        let mut universe: Vec<Value> = (-6..=6).map(int).collect();
        universe.extend((-7..=6).map(half));
        universe.extend([
            lit("", Datatype::String),
            lit("a", Datatype::String),
            lit("true", Datatype::Boolean),
        ]);
        let mut within = universe
            .iter()
            .fold(ValueSet::empty(), |s, v| s.union(&ValueSet::single(v)));
        // Also as ranges: the universe's numbers are integer[-6, 6] and halves listed.
        let numbers = restricted(
            Datatype::Integer,
            &[
                (Facet::MinInclusive, int(-6)),
                (Facet::MaxInclusive, int(6)),
            ],
        );
        within = within.union(&numbers);
        fn leaf(
            next: &mut impl FnMut(u64) -> u64,
            int: &dyn Fn(i64) -> Value,
            half: &dyn Fn(i64) -> Value,
        ) -> ValueSet {
            let k = next(13) as i64 - 6;
            let datatypes = [
                Datatype::Integer,
                Datatype::Decimal,
                Datatype::NonNegativeInteger,
                Datatype::String,
                Datatype::Boolean,
                Datatype::Float,
                Datatype::Literal,
            ];
            let facets = [
                Facet::MinInclusive,
                Facet::MaxInclusive,
                Facet::MinExclusive,
                Facet::MaxExclusive,
            ];
            match next(5) {
                0 => ValueSet::of(datatypes[next(datatypes.len() as u64) as usize]),
                1 => ValueSet::single(&int(k)),
                2 => ValueSet::single(&half(k)),
                _ => {
                    let d = [Datatype::Integer, Datatype::Decimal, Datatype::Rational]
                        [next(3) as usize];
                    let v = if next(2) == 0 { int(k) } else { half(k) };
                    ValueSet::of(d).intersection(&facet(d, facets[next(4) as usize], &v).unwrap())
                }
            }
        }
        fn tree(
            depth: u32,
            next: &mut impl FnMut(u64) -> u64,
            int: &dyn Fn(i64) -> Value,
            half: &dyn Fn(i64) -> Value,
        ) -> ValueSet {
            if depth == 0 || next(3) == 0 {
                return leaf(next, int, half);
            }
            let a = tree(depth - 1, next, int, half);
            match next(3) {
                0 => a.complement(),
                1 => a.union(&tree(depth - 1, next, int, half)),
                _ => a.intersection(&tree(depth - 1, next, int, half)),
            }
        }
        for _ in 0..2000 {
            let set = tree(4, &mut next, &int, &half).intersection(&within);
            let members: Vec<&Value> = universe.iter().filter(|v| set.contains(v)).collect();
            assert_eq!(set.count(), Count::exact(members.len() as u64), "{set:?}");
            let mut listed = set.values(100).expect("finite and known");
            listed.sort();
            let mut expected: Vec<Value> = members.into_iter().cloned().collect();
            expected.sort();
            assert_eq!(listed, expected);
        }
    }
}
