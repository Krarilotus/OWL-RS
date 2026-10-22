//! JSON-only scoped result construction. The caller owns the actual reservation.
use crate::json::{self, JsonSolutions};
use crate::{QueryResultsParseError, QueryResultsSyntaxError};
use nrese_json::{JsonSessionCallbacks, SliceJsonParser};
use nrese_rdf::{Term, Variable};
use std::{borrow::Cow, io, marker::PhantomData};

/// One invocation owner. Row ownership (and its existing charge) belongs to the
/// callback from invocation, including error/unwind cleanup. No row token escapes.
pub struct SolutionsCallbacks<C> {
    pub reserve: fn(&mut C, usize) -> io::Result<()>,
    pub free: fn(&mut C, usize),
    pub check: fn(&mut C) -> io::Result<()>,
    pub variables: fn(&mut C, &[Variable]) -> io::Result<()>,
    pub row: fn(&mut C, Vec<Option<Term>>) -> io::Result<()>,
}

pub(crate) trait Policy {
    type Bytes: Copy + Default;
    const CHECKED: bool;
    fn amount(bytes: usize) -> Self::Bytes;
    fn count(bytes: Self::Bytes) -> usize;
    fn reserve(&mut self, bytes: usize) -> Result<(), QueryResultsParseError>;
    fn free(&mut self, bytes: usize);
    fn work(&mut self, amount: usize) -> Result<(), QueryResultsParseError>;
    fn row_start(&mut self) -> Result<(), QueryResultsParseError>;
}

pub(crate) struct Unchecked;
impl Policy for Unchecked {
    type Bytes = ();
    const CHECKED: bool = false;
    #[inline]
    fn amount(_: usize) {}
    #[inline]
    fn count(_: ()) -> usize {
        0
    }
    #[inline]
    fn reserve(&mut self, _: usize) -> Result<(), QueryResultsParseError> {
        Ok(())
    }
    #[inline]
    fn free(&mut self, _: usize) {}
    #[inline]
    fn work(&mut self, _: usize) -> Result<(), QueryResultsParseError> {
        Ok(())
    }
    #[inline]
    fn row_start(&mut self) -> Result<(), QueryResultsParseError> {
        Ok(())
    }
}

/// Noop's Bytes is zero-sized: ordinary sparse slots retain their old payload size.
pub(crate) struct Held<T, P: Policy> {
    pub value: T,
    bytes: P::Bytes,
    marker: PhantomData<P>,
}
impl<T, P: Policy> Held<T, P> {
    pub fn new(value: T, bytes: usize) -> Self {
        Self {
            value,
            bytes: P::amount(bytes),
            marker: PhantomData,
        }
    }
    pub fn bytes(&self) -> usize {
        P::count(self.bytes)
    }
    pub fn parts(self) -> (T, usize) {
        (self.value, P::count(self.bytes))
    }
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Held<U, P> {
        Held {
            value: f(self.value),
            bytes: self.bytes,
            marker: PhantomData,
        }
    }
    pub fn add(&mut self, bytes: usize) {
        self.bytes = P::amount(self.bytes().checked_add(bytes).expect("admitted bytes"));
    }
}

pub(crate) fn release<T, P: Policy>(policy: &mut P, value: Held<T, P>) {
    let (value, bytes) = value.parts();
    drop(value);
    policy.free(bytes);
}

pub(crate) fn grow<T, P: Policy>(
    policy: &mut P,
    items: &mut Vec<T>,
    additional: usize,
) -> Result<(), QueryResultsParseError> {
    let required = items.len().checked_add(additional).ok_or_else(overflow)?;
    if required <= items.capacity() {
        return Ok(());
    }
    if !P::CHECKED {
        items.reserve(additional);
        return Ok(());
    }
    let capacity = required.max(items.capacity().checked_mul(2).ok_or_else(overflow)?);
    let bytes = capacity.checked_mul(size_of::<T>()).ok_or_else(overflow)?;
    let old = items
        .capacity()
        .checked_mul(size_of::<T>())
        .ok_or_else(overflow)?;
    policy.reserve(bytes)?;
    let mut next = Vec::with_capacity(capacity);
    next.append(items);
    drop(std::mem::replace(items, next));
    policy.free(old);
    Ok(())
}

pub(crate) fn text<P: Policy>(
    policy: &mut P,
    value: Cow<'_, str>,
) -> Result<Held<String, P>, QueryResultsParseError> {
    if !P::CHECKED {
        return Ok(Held::new(value.into_owned(), 0));
    }
    policy.reserve(value.len())?;
    let mut result = String::with_capacity(value.len());
    let mut at = 0;
    while at < value.len() {
        let mut end = (at + 4096).min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        policy.work(end - at)?;
        result.push_str(&value[at..end]);
        at = end;
    }
    Ok(Held::new(result, value.len()))
}

pub(crate) fn slots<P: Policy>(
    policy: &mut P,
    width: usize,
) -> Result<Held<Vec<Option<Term>>, P>, QueryResultsParseError> {
    let bytes = width
        .checked_mul(size_of::<Option<Term>>())
        .ok_or_else(overflow)?;
    policy.reserve(bytes)?;
    let mut values = Vec::with_capacity(width);
    for _ in 0..width {
        policy.work(1)?;
        values.push(None);
    }
    Ok(Held::new(values, bytes))
}

pub(crate) fn overflow() -> QueryResultsParseError {
    QueryResultsSyntaxError::msg("JSON result capacity overflow").into()
}

pub(crate) struct Owner<'a, C> {
    caller: &'a mut C,
    hooks: &'a SolutionsCallbacks<C>,
    pending: usize,
    left: usize,
    rows: usize,
    max_rows: usize,
}
impl<C> Policy for Owner<'_, C> {
    type Bytes = usize;
    const CHECKED: bool = true;
    fn amount(bytes: usize) -> usize {
        bytes
    }
    fn count(bytes: usize) -> usize {
        bytes
    }
    fn reserve(&mut self, bytes: usize) -> Result<(), QueryResultsParseError> {
        if bytes == 0 {
            return Ok(());
        }
        let pending = self.pending.checked_add(bytes).ok_or_else(overflow)?;
        (self.hooks.reserve)(self.caller, bytes)?;
        self.pending = pending;
        Ok(())
    }
    fn free(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        assert!(bytes <= self.pending, "free actual pending owner");
        (self.hooks.free)(self.caller, bytes);
        self.pending -= bytes;
    }
    #[inline]
    fn work(&mut self, amount: usize) -> Result<(), QueryResultsParseError> {
        if amount >= self.left {
            (self.hooks.check)(self.caller)?;
            self.left = 4096;
        } else {
            self.left -= amount;
        }
        Ok(())
    }
    fn row_start(&mut self) -> Result<(), QueryResultsParseError> {
        (self.hooks.check)(self.caller)?;
        if self.rows == self.max_rows {
            return Err(QueryResultsSyntaxError::msg(format!(
                "more than {} rows (federation.max_rows)",
                self.max_rows
            ))
            .into());
        }
        self.rows += 1;
        Ok(())
    }
}
impl<C> Drop for Owner<'_, C> {
    fn drop(&mut self) {
        if self.pending != 0 {
            (self.hooks.free)(self.caller, self.pending);
        }
    }
}

pub(super) fn read<C>(
    bytes: &[u8],
    max_rows: usize,
    caller: &mut C,
    hooks: &SolutionsCallbacks<C>,
) -> Result<(), QueryResultsParseError> {
    let mut owner = Owner {
        caller,
        hooks,
        pending: 0,
        left: 4096,
        rows: 0,
        max_rows,
    };
    let lexical = JsonSessionCallbacks {
        reserve: |owner: &mut Owner<'_, C>, n| owner.reserve(n).map_err(io::Error::from),
        free: |owner, n| owner.free(n),
        check: |owner| (owner.hooks.check)(owner.caller),
    };
    // Owner predates lexer startup. All parser payload locals/solutions drop
    // before lexical and finally pending credit, including callback unwind.
    SliceJsonParser::with_session(bytes, &mut owner, &lexical, |cursor| {
        let events = json::ScopedEvents(cursor);
        let (start, mut solutions) = JsonSolutions::start(events).map_err(io::Error::from)?;
        if matches!(start, json::Start::Boolean(_)) {
            return Err(io::Error::other(QueryResultsSyntaxError::msg(
                "the endpoint answered a boolean",
            ))
            .into());
        }
        let variables = solutions.variables().clone(); // Existing metadata exemption.
        solutions
            .with_owner(|owner| {
                (owner.hooks.variables)(owner.caller, &variables)
                    .map_err(QueryResultsParseError::from)
            })
            .map_err(io::Error::from)?;
        while let Some(row) = solutions.next_held().map_err(io::Error::from)? {
            let (values, bytes) = row.parts();
            solutions
                .with_owner(|owner| {
                    assert!(bytes <= owner.pending);
                    owner.pending -= bytes; // Ownership changes; no credit release/readmission.
                    (owner.hooks.row)(owner.caller, values).map_err(QueryResultsParseError::from)
                })
                .map_err(io::Error::from)?;
        }
        Ok(())
    })
    .map_err(from_json)
}

pub(crate) fn from_json(error: nrese_json::JsonParseError) -> QueryResultsParseError {
    match error {
        nrese_json::JsonParseError::Io(error) => {
            match error.downcast::<QueryResultsSyntaxError>() {
                Ok(error) => QueryResultsParseError::Syntax(error),
                Err(error) => QueryResultsParseError::Io(error),
            }
        }
        nrese_json::JsonParseError::Syntax(error) => error.into(),
    }
}
