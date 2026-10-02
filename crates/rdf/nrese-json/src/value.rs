//! The JSON tree: strings and numbers borrowed from the text where they can be, object
//! entries in document order.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;

use crate::error::JsonSyntaxError;
use crate::parser::SliceJsonParser;
use crate::writer::write_value;

/// A JSON value. A number keeps its text (checked against the grammar): its value is
/// the reader's business, and the text loses nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Value<'a> {
    #[default]
    Null,
    Boolean(bool),
    Number(Cow<'a, str>),
    String(Cow<'a, str>),
    Array(Vec<Value<'a>>),
    Object(Object<'a>),
}

/// A JSON object: its entries in document order, each key once (for a key written twice,
/// the last entry counts, as in ECMAScript's `JSON.parse`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Object<'a> {
    entries: Vec<(Cow<'a, str>, Value<'a>)>,
}

impl<'a> Value<'a> {
    /// The document `text`.
    pub fn parse(text: &'a str) -> Result<Self, JsonSyntaxError> {
        Self::from_parser(SliceJsonParser::new(text))
    }

    /// The document in `bytes`, which must be UTF-8.
    pub fn parse_bytes(bytes: &'a [u8]) -> Result<Self, JsonSyntaxError> {
        Self::from_parser(SliceJsonParser::from_bytes(bytes)?)
    }

    fn from_parser(mut parser: SliceJsonParser<'a>) -> Result<Self, JsonSyntaxError> {
        let value = parser.next_value()?.unwrap_or_default();
        // The parser checks that nothing follows.
        parser.next_event()?;
        Ok(value)
    }

    pub fn into_owned(self) -> Value<'static> {
        match self {
            Self::Null => Value::Null,
            Self::Boolean(b) => Value::Boolean(b),
            Self::Number(n) => Value::Number(Cow::Owned(n.into_owned())),
            Self::String(s) => Value::String(Cow::Owned(s.into_owned())),
            Self::Array(items) => Value::Array(items.into_iter().map(Value::into_owned).collect()),
            Self::Object(object) => Value::Object(object.into_owned()),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// A string, a number, or a boolean.
    pub fn is_scalar(&self) -> bool {
        matches!(self, Self::String(_) | Self::Number(_) | Self::Boolean(_))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value<'a>]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Object<'a>> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
        }
    }

    /// A number's value as the nearest double (correctly rounded); `None` if not a number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => n.parse().ok(),
            _ => None,
        }
    }

    /// The value as one item, or the items of an array.
    pub fn as_items(&self) -> &[Value<'a>] {
        match self {
            Self::Array(items) => items,
            other => std::slice::from_ref(other),
        }
    }
}

impl fmt::Display for Value<'_> {
    /// Compact JSON.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        write_value(self, &mut out);
        f.write_str(&out)
    }
}

impl From<bool> for Value<'_> {
    fn from(b: bool) -> Self {
        Self::Boolean(b)
    }
}

impl<'a> From<&'a str> for Value<'a> {
    fn from(s: &'a str) -> Self {
        Self::String(Cow::Borrowed(s))
    }
}

impl From<String> for Value<'_> {
    fn from(s: String) -> Self {
        Self::String(Cow::Owned(s))
    }
}

impl<'a> From<Object<'a>> for Value<'a> {
    fn from(object: Object<'a>) -> Self {
        Self::Object(object)
    }
}

impl<'a> From<Vec<Value<'a>>> for Value<'a> {
    fn from(items: Vec<Value<'a>>) -> Self {
        Self::Array(items)
    }
}

impl<'a> Object<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value<'a>> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value<'a>> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    /// Sets `key` to `value`: in place if the key is there, otherwise at the end.
    pub fn insert(&mut self, key: impl Into<Cow<'a, str>>, value: Value<'a>) {
        let key = key.into();
        match self.get_mut(&key) {
            Some(slot) => *slot = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Value<'a>> {
        let at = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(at).1)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &Value<'a>)> {
        self.entries.iter().map(|(k, v)| (k.as_ref(), v))
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_ref())
    }

    pub fn into_owned(self) -> Object<'static> {
        Object {
            entries: self
                .entries
                .into_iter()
                .map(|(k, v)| (Cow::Owned(k.into_owned()), v.into_owned()))
                .collect(),
        }
    }

    /// Appends without looking for the key (the parser removes duplicates at the end).
    pub(crate) fn push(&mut self, key: Cow<'a, str>, value: Value<'a>) {
        self.entries.push((key, value));
    }

    /// Keeps the last entry of each key, in linear time.
    pub(crate) fn keep_last_duplicates(&mut self) {
        let n = self.entries.len();
        if n < 2 {
            return;
        }
        let duplicated = if n <= 8 {
            (1..n).any(|i| {
                self.entries[..i]
                    .iter()
                    .any(|(k, _)| *k == self.entries[i].0)
            })
        } else {
            let mut seen = HashSet::with_capacity(n);
            !self.entries.iter().all(|(k, _)| seen.insert(k.as_ref()))
        };
        if !duplicated {
            return;
        }
        let mut seen = HashSet::with_capacity(n);
        let mut keep = vec![false; n];
        for (i, (k, _)) in self.entries.iter().enumerate().rev() {
            keep[i] = seen.insert(k.clone());
        }
        let mut flags = keep.into_iter();
        self.entries.retain(|_| flags.next().unwrap_or(false));
    }
}

impl<'a> IntoIterator for Object<'a> {
    type Item = (Cow<'a, str>, Value<'a>);
    type IntoIter = std::vec::IntoIter<(Cow<'a, str>, Value<'a>)>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a, K: Into<Cow<'a, str>>> FromIterator<(K, Value<'a>)> for Object<'a> {
    fn from_iter<T: IntoIterator<Item = (K, Value<'a>)>>(iter: T) -> Self {
        let mut object = Object::new();
        for (k, v) in iter {
            object.insert(k, v);
        }
        object
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_borrows_and_keeps_order() {
        let text = r#"{"b": 1, "a": "x", "c": "y\"", "b": [true, null]}"#;
        let value = Value::parse(text).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.keys().collect::<Vec<_>>(), ["a", "c", "b"]);
        assert!(matches!(
            object.get("a"),
            Some(Value::String(Cow::Borrowed("x")))
        ));
        assert!(matches!(
            object.get("c"),
            Some(Value::String(Cow::Owned(_)))
        ));
        assert_eq!(value.to_string(), r#"{"a":"x","c":"y\"","b":[true,null]}"#);
        assert!(Value::parse("[1] x").is_err());
        assert_eq!(Value::parse(" 2.5e1 ").unwrap().as_f64(), Some(25.0));
    }

    #[test]
    fn many_duplicates() {
        let text = format!(
            "{{{}}}",
            (0..20)
                .map(|i| format!("\"k{}\": {i}", i % 5))
                .collect::<Vec<_>>()
                .join(",")
        );
        let value = Value::parse(&text).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 5);
        assert_eq!(object.get("k0"), Some(&Value::Number("15".into())));
    }
}
