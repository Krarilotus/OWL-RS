//! The JSON-LD reader: a document in, quads out, one top-level element at a time where
//! that is exact.
//!
//! A top-level array's elements are independent: each is expanded and converted alone.
//! A top-level object is first read key by key, its values skipped (but checked) except
//! `@context`; if its only other key expands to `@graph`, the document is that graph,
//! and its elements are expanded and converted one at a time under the context. Any
//! other document is read whole. Memory is the text plus one element's tree.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use nrese_json::{JsonEvent, JsonParserState, JsonSyntaxError, SliceJsonParser, Value};
use nrese_rdf::{Iri, QuadRef};

use super::context::{Context, Processor};
use super::expand::Expander;
use super::items::Item;
use super::to_rdf::{BlankNames, Emitter, QuadArena};
use super::{JsonLdError, JsonLdErrorCode, JsonLdOptions};
use crate::blank::BlankNodes;
use crate::error::{RdfParseError, RdfSyntaxError, TextPosition};

/// The settings of [`crate::RdfParser`] JSON-LD uses.
pub(crate) struct JsonLdSettings {
    pub(crate) base: Option<Iri<String>>,
    pub(crate) blank_nodes: BlankNodes,
    pub(crate) unchecked: bool,
    pub(crate) max_depth: usize,
    pub(crate) options: JsonLdOptions,
    /// After an element of a streamed top-level array (or `@graph` array) fails to
    /// expand, go on with the next ([`crate::RdfParser::recovering`]).
    pub(crate) recover: bool,
}

enum State {
    Start,
    /// Elements of an array, expanded under `active` with `property` (`None` at the top
    /// level, `@graph` in a top-level graph).
    Items {
        parser: JsonParserState,
        active: Arc<Context>,
        property: Option<&'static str>,
    },
    Done,
}

/// The quads of one JSON-LD document.
pub(crate) struct JsonLdParser<'a> {
    text: Result<Cow<'a, str>, Option<RdfParseError>>,
    core: Core,
}

/// Everything but the text, so that the text can be borrowed while the rest changes.
struct Core {
    state: State,
    processor: Processor,
    names: BlankNames,
    base: Option<Arc<Iri<String>>>,
    unchecked: bool,
    max_depth: usize,
    recover: bool,
    arena: QuadArena,
    next: usize,
}

fn syntax(error: JsonSyntaxError) -> RdfParseError {
    let at = TextPosition {
        line: error.line(),
        column: error.column(),
        offset: error.offset(),
    };
    RdfSyntaxError::new(
        format!(
            "{}: {}",
            JsonLdErrorCode::LoadingDocumentFailed,
            error.message()
        ),
        at..at,
    )
    .into()
}

fn processing(error: JsonLdError) -> RdfParseError {
    RdfSyntaxError::new(
        error.to_string(),
        TextPosition::default()..TextPosition::default(),
    )
    .into()
}

impl<'a> JsonLdParser<'a> {
    /// The parser of `bytes`, which must be UTF-8 (checked here, once).
    pub(crate) fn new(bytes: Cow<'a, [u8]>, settings: JsonLdSettings) -> Self {
        let text = match bytes {
            Cow::Borrowed(bytes) => {
                SliceJsonParser::from_bytes(bytes).map(|p| Cow::Borrowed(p.text()))
            }
            Cow::Owned(bytes) => String::from_utf8(bytes).map(Cow::Owned).map_err(|e| {
                let at = e.utf8_error().valid_up_to();
                // The position, as the slice parser reports it.
                SliceJsonParser::from_bytes(&e.as_bytes()[..=at.min(e.as_bytes().len() - 1)])
                    .err()
                    .unwrap_or_else(|| unreachable!("invalid UTF-8 up to {at}"))
            }),
        };
        Self {
            text: text.map_err(|e| Some(syntax(e))),
            core: Core {
                state: State::Start,
                processor: Processor::new(settings.options),
                names: BlankNames::new(settings.blank_nodes),
                base: settings.base.map(Arc::new),
                unchecked: settings.unchecked,
                max_depth: settings.max_depth,
                recover: settings.recover,
                arena: QuadArena::default(),
                next: 0,
            },
        }
    }

    pub(crate) fn next_ref(&mut self) -> Option<Result<QuadRef<'_>, RdfParseError>> {
        match self.fill() {
            Ok(true) => {
                let core = &mut self.core;
                let i = core.next;
                core.next += 1;
                Some(Ok(core.arena.quad(i)))
            }
            Ok(false) => None,
            // The state says whether to go on: a step that fails leaves it `Done`, unless
            // it skipped one element of a stream and recovers.
            Err(error) => Some(Err(error)),
        }
    }

    /// Makes quads ready; `false` at the end of the document.
    fn fill(&mut self) -> Result<bool, RdfParseError> {
        let core = &mut self.core;
        while core.next >= core.arena.len() {
            if matches!(core.state, State::Done) {
                return Ok(false);
            }
            core.arena.clear();
            core.next = 0;
            let text: &str = match &mut self.text {
                Ok(text) => text,
                Err(error) => {
                    core.state = State::Done;
                    return Err(error.take().unwrap_or_else(|| {
                        RdfSyntaxError::new("the document isn't UTF-8", Default::default()).into()
                    }));
                }
            };
            core.step(text)?;
        }
        Ok(true)
    }
}

impl Core {
    /// One element's quads into the arena (perhaps none), or the end.
    fn step(&mut self, text: &str) -> Result<(), RdfParseError> {
        match std::mem::replace(&mut self.state, State::Done) {
            State::Done => {}
            State::Start => {
                let mut parser = SliceJsonParser::new(text).with_max_depth(self.max_depth);
                match parser.next_event().map_err(syntax)? {
                    JsonEvent::StartArray => {
                        let active = self
                            .processor
                            .initial_context(self.base.clone())
                            .map_err(processing)?;
                        self.state = State::Items {
                            parser: parser.state(),
                            active,
                            property: None,
                        };
                    }
                    JsonEvent::StartObject => {
                        let start = parser.token_start();
                        self.top_object(text, parser, start)?;
                    }
                    _ => {
                        // A scalar: nothing, but the rest must be JSON.
                        parser.next_event().map_err(syntax)?;
                    }
                }
            }
            State::Items {
                parser,
                active,
                property,
            } => {
                let mut parser = SliceJsonParser::resume(text, parser);
                match parser.next_value().map_err(syntax)? {
                    None => {
                        parser.next_event().map_err(syntax)?;
                    }
                    Some(element) => {
                        let expanded = Expander::new(&mut self.processor)
                            .with_base_url(self.base.clone())
                            .expand(&active, property, &element, false)
                            .map(|items| items.into_vec());
                        // The element was read whole: a recovering parser skips it and
                        // goes on with the next.
                        if expanded.is_ok() || self.recover {
                            self.state = State::Items {
                                parser: parser.state(),
                                active,
                                property,
                            };
                        }
                        self.emit(&expanded.map_err(processing)?);
                    }
                }
            }
        }
        Ok(())
    }

    /// A top-level object: streamed if it is only a graph, read whole otherwise.
    fn top_object(
        &mut self,
        text: &str,
        mut parser: SliceJsonParser<'_>,
        start: usize,
    ) -> Result<(), RdfParseError> {
        let mut context = None;
        let mut others: Vec<(Cow<'_, str>, Range<usize>)> = Vec::new();
        loop {
            match parser.next_event().map_err(syntax)? {
                JsonEvent::ObjectKey(key) if key == "@context" => {
                    context = parser.next_value().map_err(syntax)?;
                }
                JsonEvent::ObjectKey(key) => {
                    let range = parser
                        .skip_value()
                        .map_err(syntax)?
                        .ok_or_else(|| syntax(parser.error("a value should follow the key")))?;
                    others.push((key, range));
                }
                _ => break,
            }
        }
        parser.next_event().map_err(syntax)?;
        let initial = self
            .processor
            .initial_context(self.base.clone())
            .map_err(processing)?;
        let active = match &context {
            Some(local) => Arc::new(
                self.processor
                    .process(
                        &initial,
                        local,
                        self.base.as_ref(),
                        &mut Vec::new(),
                        false,
                        true,
                        true,
                    )
                    .map_err(processing)?,
            ),
            None => initial.clone(),
        };
        let graph_only = matches!(others.as_slice(), [(key, _)]
            if active.expand_key(key).is_some_and(|e| e.as_str() == "@graph"));
        if graph_only {
            let range = others[0].1.clone();
            let mut graph =
                SliceJsonParser::value_at(text, range.start).with_max_depth(self.max_depth);
            if text.as_bytes()[range.start] == b'[' {
                graph.next_event().map_err(syntax)?;
                self.state = State::Items {
                    parser: graph.state(),
                    active,
                    property: Some("@graph"),
                };
                return Ok(());
            }
            let element = graph.next_value().map_err(syntax)?.unwrap_or_default();
            let items = Expander::new(&mut self.processor)
                .with_base_url(self.base.clone())
                .expand(&active, Some("@graph"), &element, false)
                .map_err(processing)?
                .into_vec();
            self.emit(&items);
            return Ok(());
        }
        let document = SliceJsonParser::value_at(text, start)
            .with_max_depth(self.max_depth)
            .next_value()
            .map_err(syntax)?
            .unwrap_or(Value::Null);
        let items = Expander::new(&mut self.processor)
            .with_base_url(self.base.clone())
            .expand_document(&initial, &document)
            .map_err(processing)?;
        self.emit(&items);
        Ok(())
    }

    fn emit(&mut self, items: &[Item]) {
        let mut emitter = Emitter {
            arena: &mut self.arena,
            names: &mut self.names,
            rdf_direction: self.processor.options.rdf_direction,
            unchecked: self.unchecked,
            scratch: String::new(),
        };
        emitter.document(items);
    }
}
