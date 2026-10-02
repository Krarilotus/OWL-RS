//! Writing JSON: escaping, a tree as compact text, and a streaming writer of events.

use std::io::{self, Write};

use crate::parser::JsonEvent;
use crate::value::Value;

/// Bytes a JSON string can't hold as they are: `"`, `\` and the control characters.
const fn needs_escape() -> [bool; 256] {
    let mut table = [false; 256];
    let mut b = 0;
    while b < 0x20 {
        table[b] = true;
        b += 1;
    }
    table[b'"' as usize] = true;
    table[b'\\' as usize] = true;
    table
}
const NEEDS_ESCAPE: [bool; 256] = needs_escape();

/// `text` as a JSON string, quotes included, handed out in pieces. Only what must be
/// escaped is (as RFC 8785 asks): `"`, `\`, and control characters, the short forms where
/// there are some and `\u00xx` in lower case otherwise.
pub fn escape(text: &str, mut push: impl FnMut(&str)) {
    push("\"");
    let bytes = text.as_bytes();
    let mut run = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if !NEEDS_ESCAPE[b as usize] {
            continue;
        }
        // Escaped bytes are ASCII, so `i` is a character boundary.
        push(&text[run..i]);
        run = i + 1;
        let short = match b {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            0x08 => "\\b",
            0x0C => "\\f",
            _ => "",
        };
        if short.is_empty() {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let code = [
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[(b >> 4) as usize],
                HEX[(b & 15) as usize],
            ];
            push(std::str::from_utf8(&code).unwrap_or_default());
        } else {
            push(short);
        }
    }
    push(&text[run..]);
    push("\"");
}

/// Appends `text` as a JSON string.
pub fn write_string(text: &str, out: &mut String) {
    escape(text, |piece| out.push_str(piece));
}

/// Appends `value` as compact JSON. Iterative: depth costs no stack.
pub fn write_value(value: &Value<'_>, out: &mut String) {
    enum Step<'v, 'a> {
        Value(&'v Value<'a>),
        Text(&'static str),
        Key(&'v str),
    }
    let mut todo = vec![Step::Value(value)];
    while let Some(step) = todo.pop() {
        match step {
            Step::Text(text) => out.push_str(text),
            Step::Key(key) => {
                write_string(key, out);
                out.push(':');
            }
            Step::Value(value) => match value {
                Value::Null => out.push_str("null"),
                Value::Boolean(true) => out.push_str("true"),
                Value::Boolean(false) => out.push_str("false"),
                Value::Number(n) => out.push_str(n),
                Value::String(s) => write_string(s, out),
                Value::Array(items) => {
                    out.push('[');
                    todo.push(Step::Text("]"));
                    for (i, item) in items.iter().enumerate().rev() {
                        todo.push(Step::Value(item));
                        if i > 0 {
                            todo.push(Step::Text(","));
                        }
                    }
                }
                Value::Object(object) => {
                    out.push('{');
                    todo.push(Step::Text("}"));
                    let entries: Vec<_> = object.iter().collect();
                    for (i, (key, item)) in entries.into_iter().enumerate().rev() {
                        todo.push(Step::Value(item));
                        todo.push(Step::Key(key));
                        if i > 0 {
                            todo.push(Step::Text(","));
                        }
                    }
                }
            },
        }
    }
}

/// Writes events as compact JSON, buffered, checking that they make a document.
pub struct JsonWriter<W: Write> {
    out: W,
    buffer: Vec<u8>,
    /// The open containers: whether it is an object, and whether it has an entry yet.
    stack: Vec<(bool, bool)>,
    after_key: bool,
    done: bool,
}

const FLUSH_AT: usize = 64 * 1024;

fn misuse(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
}

impl<W: Write> JsonWriter<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            buffer: Vec::with_capacity(FLUSH_AT),
            stack: Vec::new(),
            after_key: false,
            done: false,
        }
    }

    fn before_value(&mut self) -> io::Result<()> {
        if self.done {
            return Err(misuse("the document already has its value"));
        }
        if self.after_key {
            self.after_key = false;
            return Ok(());
        }
        match self.stack.last_mut() {
            Some((true, _)) => Err(misuse("a value in an object needs a key first")),
            Some((false, started)) => {
                if *started {
                    self.buffer.push(b',');
                }
                *started = true;
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn after_value(&mut self) -> io::Result<()> {
        if self.stack.is_empty() {
            self.done = true;
        }
        if self.buffer.len() >= FLUSH_AT {
            self.out.write_all(&self.buffer)?;
            self.buffer.clear();
        }
        Ok(())
    }

    fn string(&mut self, text: &str) {
        let buffer = &mut self.buffer;
        escape(text, |piece| buffer.extend_from_slice(piece.as_bytes()));
    }

    pub fn write_event(&mut self, event: JsonEvent<'_>) -> io::Result<()> {
        match event {
            JsonEvent::String(s) => {
                self.before_value()?;
                self.string(&s);
                self.after_value()
            }
            JsonEvent::Number(n) => {
                self.before_value()?;
                self.buffer.extend_from_slice(n.as_bytes());
                self.after_value()
            }
            JsonEvent::Boolean(b) => {
                self.before_value()?;
                self.buffer
                    .extend_from_slice(if b { b"true" } else { b"false" });
                self.after_value()
            }
            JsonEvent::Null => {
                self.before_value()?;
                self.buffer.extend_from_slice(b"null");
                self.after_value()
            }
            JsonEvent::StartArray | JsonEvent::StartObject => {
                self.before_value()?;
                let object = event == JsonEvent::StartObject;
                self.buffer.push(if object { b'{' } else { b'[' });
                self.stack.push((object, false));
                Ok(())
            }
            JsonEvent::EndArray | JsonEvent::EndObject => {
                let object = event == JsonEvent::EndObject;
                match self.stack.pop() {
                    Some((o, _)) if o == object && !self.after_key => {}
                    _ => return Err(misuse("a closing bracket that doesn't match")),
                }
                self.buffer.push(if object { b'}' } else { b']' });
                self.after_value()
            }
            JsonEvent::ObjectKey(key) => {
                match self.stack.last_mut() {
                    Some((true, started)) if !self.after_key => {
                        if *started {
                            self.buffer.push(b',');
                        }
                        *started = true;
                    }
                    _ => return Err(misuse("a key outside an object")),
                }
                self.string(&key);
                self.buffer.push(b':');
                self.after_key = true;
                Ok(())
            }
            JsonEvent::Eof => Ok(()),
        }
    }

    /// Writes what is buffered and gives the writer back; the document must be complete.
    pub fn finish(mut self) -> io::Result<W> {
        if !self.done {
            return Err(misuse("the document isn't complete"));
        }
        self.out.write_all(&self.buffer)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_only_what_it_must() {
        let mut out = String::new();
        write_string("a\"b\\c\n\u{1}\u{7f}/é😀", &mut out);
        assert_eq!(out, "\"a\\\"b\\\\c\\n\\u0001\u{7f}/é😀\"");
    }

    #[test]
    fn writer_makes_documents() {
        let mut writer = JsonWriter::new(Vec::new());
        for event in [
            JsonEvent::StartObject,
            JsonEvent::ObjectKey("a".into()),
            JsonEvent::StartArray,
            JsonEvent::Number("1".into()),
            JsonEvent::String("x".into()),
            JsonEvent::StartObject,
            JsonEvent::EndObject,
            JsonEvent::EndArray,
            JsonEvent::ObjectKey("b".into()),
            JsonEvent::Null,
            JsonEvent::EndObject,
        ] {
            writer.write_event(event).unwrap();
        }
        assert_eq!(writer.finish().unwrap(), b"{\"a\":[1,\"x\",{}],\"b\":null}");
        let mut writer = JsonWriter::new(Vec::new());
        writer.write_event(JsonEvent::StartObject).unwrap();
        assert!(writer.write_event(JsonEvent::Null).is_err());
        assert!(JsonWriter::new(Vec::new()).finish().is_err());
    }
}
