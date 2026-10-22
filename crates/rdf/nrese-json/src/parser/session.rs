//! Scoped allocation/control for the existing slice lexer. UTF-8 validation and
//! invalid-prefix location still use the ordinary entry and are explicitly unported.

use super::{JsonEvent, JsonParseError, JsonSyntaxError, Lexer, SliceJsonParser};
use std::{borrow::Cow, io};

/// Invocation callbacks. Reserve admits actual capacity before allocation; free
/// follows payload destruction and must not panic. No owner is retained elsewhere.
pub struct JsonSessionCallbacks<C> {
    pub reserve: fn(&mut C, usize) -> io::Result<()>,
    pub free: fn(&mut C, usize),
    pub check: fn(&mut C) -> io::Result<()>,
}

pub(super) trait Control {
    type Error: From<JsonSyntaxError>;
    const CHECKED: bool;
    fn work(&mut self, amount: usize) -> Result<(), Self::Error>;
    fn grow_stack(&mut self, stack: &mut Vec<bool>) -> Result<(), Self::Error>;
    fn text(&mut self, bytes: usize) -> Result<(), Self::Error>;
}

pub(super) struct Unchecked;
impl Control for Unchecked {
    type Error = JsonSyntaxError;
    const CHECKED: bool = false;
    #[inline]
    fn work(&mut self, _: usize) -> Result<(), Self::Error> {
        Ok(())
    }
    #[inline]
    fn grow_stack(&mut self, _: &mut Vec<bool>) -> Result<(), Self::Error> {
        Ok(())
    }
    #[inline]
    fn text(&mut self, _: usize) -> Result<(), Self::Error> {
        Ok(())
    }
}

const BATCH: usize = 4096;

/// A scoped cursor. It never exports owned decoded text or a parser-state clone.
pub struct SliceJsonSession<'input, 'scope, C> {
    text: &'input str,
    pos: usize,
    lexer: Lexer,
    owner: &'scope mut C,
    callbacks: &'scope JsonSessionCallbacks<C>,
    stack_held: usize,
    left: usize,
}

impl<'input> SliceJsonParser<'input> {
    /// Uses the existing lexer within one invocation. Initial UTF-8 validation
    /// and its invalid-input location scan are not internally interruptible yet.
    pub fn with_session<C, R>(
        bytes: &'input [u8],
        owner: &mut C,
        callbacks: &JsonSessionCallbacks<C>,
        run: impl for<'scope> FnOnce(
            &mut SliceJsonSession<'input, 'scope, C>,
        ) -> Result<R, JsonParseError>,
    ) -> Result<R, JsonParseError> {
        (callbacks.check)(owner)?;
        let parser = Self::from_bytes(bytes)?;
        (callbacks.check)(owner)?;
        let mut session = SliceJsonSession {
            text: parser.text,
            pos: parser.pos,
            lexer: parser.lexer,
            owner,
            callbacks,
            stack_held: 0,
            left: BATCH,
        };
        let result = run(&mut session);
        (session.callbacks.check)(session.owner)?;
        result
    }
}

struct EventControl<'a, C> {
    owner: &'a mut C,
    callbacks: &'a JsonSessionCallbacks<C>,
    stack_held: &'a mut usize,
    left: &'a mut usize,
    text_held: usize,
}

impl<C> Control for EventControl<'_, C> {
    type Error = JsonParseError;
    const CHECKED: bool = true;
    #[inline]
    fn work(&mut self, amount: usize) -> Result<(), Self::Error> {
        if amount >= *self.left {
            (self.callbacks.check)(self.owner)?;
            *self.left = BATCH;
        } else {
            *self.left -= amount;
        }
        Ok(())
    }
    fn grow_stack(&mut self, stack: &mut Vec<bool>) -> Result<(), Self::Error> {
        if stack.len() != stack.capacity() {
            return Ok(());
        }
        let old = stack.capacity();
        let capacity = old
            .checked_mul(2)
            .map(|n| n.max(4))
            .ok_or_else(|| io::Error::other("JSON stack capacity overflow"))?;
        (self.callbacks.reserve)(self.owner, capacity)?;
        *self.stack_held += capacity;
        let mut next = Vec::with_capacity(capacity);
        next.append(stack);
        drop(std::mem::replace(stack, next));
        if old != 0 {
            (self.callbacks.free)(self.owner, old);
        }
        *self.stack_held -= old;
        Ok(())
    }
    fn text(&mut self, bytes: usize) -> Result<(), Self::Error> {
        (self.callbacks.reserve)(self.owner, bytes)?;
        self.text_held += bytes;
        Ok(())
    }
}

struct EventGuard<'input, 'a, C> {
    event: Option<JsonEvent<'input>>,
    control: EventControl<'a, C>,
}
impl<C> Drop for EventGuard<'_, '_, C> {
    fn drop(&mut self) {
        drop(self.event.take());
        if self.control.text_held != 0 {
            (self.control.callbacks.free)(self.control.owner, self.control.text_held);
        }
    }
}

impl<'input, C> SliceJsonSession<'input, '_, C> {
    /// Consume a borrowing view. An owned copy made by consume belongs to its
    /// caller, which must admit it separately before allocation.
    ///
    /// ```compile_fail
    /// use nrese_json::{JsonEvent, JsonSessionCallbacks, SliceJsonParser};
    /// let hooks = JsonSessionCallbacks {
    ///     reserve: |_: &mut (), _| Ok(()), free: |_, _| {}, check: |_| Ok(()),
    /// };
    /// let escaped: &str = SliceJsonParser::with_session(br#""a\nb""#, &mut (), &hooks, |cursor| {
    ///     cursor.next_event(|_, event| match event {
    ///         JsonEvent::String(text) => Ok(text.as_ref()),
    ///         _ => unreachable!(),
    ///     })
    /// }).unwrap();
    /// println!("{escaped}");
    /// ```
    pub fn next_event<R>(
        &mut self,
        consume: impl for<'event> FnOnce(&mut C, JsonEvent<'event>) -> Result<R, JsonParseError>,
    ) -> Result<R, JsonParseError> {
        let mut guard = EventGuard {
            event: None,
            control: EventControl {
                owner: self.owner,
                callbacks: self.callbacks,
                stack_held: &mut self.stack_held,
                left: &mut self.left,
                text_held: 0,
            },
        };
        guard.control.work(1)?;
        let raw = self
            .lexer
            .step(self.text, &mut self.pos, true, &mut guard.control)?
            .expect("slice has all input");
        guard.event = Some(self.lexer.event(self.text, raw, &mut guard.control)?);
        let view = borrowed(guard.event.as_ref().expect("event constructed"));
        consume(guard.control.owner, view)
    }
    pub fn owner_mut(&mut self) -> &mut C {
        self.owner
    }
}

impl<C> Drop for SliceJsonSession<'_, '_, C> {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.lexer.stack));
        (self.callbacks.free)(self.owner, self.stack_held);
    }
}

fn borrowed<'a>(event: &'a JsonEvent<'_>) -> JsonEvent<'a> {
    match event {
        JsonEvent::String(s) => JsonEvent::String(Cow::Borrowed(s)),
        JsonEvent::ObjectKey(s) => JsonEvent::ObjectKey(Cow::Borrowed(s)),
        JsonEvent::Number(s) => JsonEvent::Number(Cow::Borrowed(s)),
        JsonEvent::Boolean(v) => JsonEvent::Boolean(*v),
        JsonEvent::Null => JsonEvent::Null,
        JsonEvent::StartArray => JsonEvent::StartArray,
        JsonEvent::EndArray => JsonEvent::EndArray,
        JsonEvent::StartObject => JsonEvent::StartObject,
        JsonEvent::EndObject => JsonEvent::EndObject,
        JsonEvent::Eof => JsonEvent::Eof,
    }
}

pub(super) fn copy<P: Control>(
    text: &str,
    out: &mut String,
    control: &mut P,
) -> Result<(), P::Error> {
    if !P::CHECKED {
        out.push_str(text);
        return Ok(());
    }
    let mut at = 0;
    while at < text.len() {
        let mut end = (at + BATCH).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        control.work(end - at)?;
        out.push_str(&text[at..end]);
        at = end;
    }
    Ok(())
}

pub(super) fn slash<P: Control>(bytes: &[u8], control: &mut P) -> Result<Option<usize>, P::Error> {
    if !P::CHECKED {
        return Ok(memchr::memchr(b'\\', bytes));
    }
    for (i, chunk) in bytes.chunks(BATCH).enumerate() {
        control.work(chunk.len())?;
        if let Some(at) = memchr::memchr(b'\\', chunk) {
            return Ok(Some(i * BATCH + at));
        }
    }
    Ok(None)
}
