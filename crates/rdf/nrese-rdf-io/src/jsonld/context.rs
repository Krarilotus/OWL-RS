//! Active contexts and term definitions: the Context Processing and Create Term Definition
//! algorithms (JSON-LD 1.1 API §4.1, §4.2) and IRI Expansion (§5.2).
//!
//! An active context is cheap to clone: its term table is shared and copied on write.
//! Applying a term's scoped context to an active context is memoised, as in data every
//! node of a type, or every value of a property, applies the same one to the same context.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use nrese_json::{Object, Value};
use nrese_rdf::Iri;

use super::{JsonLdError, JsonLdErrorCode as Code, JsonLdOptions, JsonLdProcessingMode};

/// IRIs, blank node identifiers and keywords as the expanded form shares them.
pub(crate) type Str = Arc<str>;

type Map<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

/// How many remote contexts may be open at once: deeper is a loop or an attack.
const MAX_REMOTE_CONTEXTS: usize = 32;

/// How many scoped-context applications are remembered before the memo starts over.
const MEMO_SIZE: usize = 4096;

pub(crate) fn error(code: Code, message: impl Into<String>) -> JsonLdError {
    JsonLdError::new(code, message)
}

/// The keywords of JSON-LD 1.1 (and of framing, which expansion must recognise).
pub(crate) fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "@base"
            | "@container"
            | "@context"
            | "@default"
            | "@direction"
            | "@embed"
            | "@explicit"
            | "@graph"
            | "@id"
            | "@import"
            | "@included"
            | "@index"
            | "@json"
            | "@language"
            | "@list"
            | "@nest"
            | "@none"
            | "@omitDefault"
            | "@prefix"
            | "@preserve"
            | "@propagate"
            | "@protected"
            | "@requireAll"
            | "@reverse"
            | "@set"
            | "@type"
            | "@value"
            | "@version"
            | "@vocab"
    )
}

/// `"@"1*ALPHA`: reserved for keywords, ignored where it isn't one.
pub(crate) fn has_keyword_form(s: &str) -> bool {
    s.len() > 1 && s.starts_with('@') && s[1..].bytes().all(|b| b.is_ascii_alphabetic())
}

/// Whether `s` starts with a scheme and a colon: the form of an absolute IRI (whether
/// it is well formed is checked when it becomes RDF).
pub(crate) fn has_iri_form(s: &str) -> bool {
    let Some(colon) = s.find(':') else {
        return false;
    };
    let scheme = &s.as_bytes()[..colon];
    scheme.first().is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// An IRI or a blank node identifier (`_:`), the forms a term may map to.
fn is_iri_or_blank(s: &str) -> bool {
    s.starts_with("_:") || has_iri_form(s)
}

/// The position of the colon that makes `s` a compact IRI or an IRI: the first one, if
/// it isn't the first character.
pub(crate) fn prefix_colon(s: &str) -> Option<usize> {
    s.find(':').filter(|&i| i > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Ltr,
    Rtl,
}

impl Direction {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "ltr" => Some(Self::Ltr),
            "rtl" => Some(Self::Rtl),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }
}

/// A term's container mapping, as flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Container(u8);

impl Container {
    pub(crate) const GRAPH: u8 = 1;
    pub(crate) const ID: u8 = 2;
    pub(crate) const INDEX: u8 = 4;
    pub(crate) const LANGUAGE: u8 = 8;
    pub(crate) const LIST: u8 = 16;
    pub(crate) const SET: u8 = 32;
    pub(crate) const TYPE: u8 = 64;

    pub(crate) fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    fn flag(keyword: &str) -> Option<u8> {
        Some(match keyword {
            "@graph" => Self::GRAPH,
            "@id" => Self::ID,
            "@index" => Self::INDEX,
            "@language" => Self::LANGUAGE,
            "@list" => Self::LIST,
            "@set" => Self::SET,
            "@type" => Self::TYPE,
            _ => return None,
        })
    }

    /// The combinations §4.2 step 21.1 allows.
    fn is_valid(self) -> bool {
        let flags = self.0;
        if flags == 0 {
            return false;
        }
        if self.has(Self::LIST) {
            return flags == Self::LIST;
        }
        if self.has(Self::GRAPH) {
            let others = flags & !(Self::GRAPH | Self::SET);
            return others == 0 || others == Self::ID || others == Self::INDEX;
        }
        (flags & !Self::SET).count_ones() <= 1
    }
}

/// A term definition (§4.2).
#[derive(Debug, Clone, Default)]
pub(crate) struct TermDefinition {
    /// The IRI mapping: an IRI, a blank node identifier or a keyword; `None` for a term
    /// defined as `null` (kept, so that redefining a protected one is detected).
    pub(crate) iri: Option<Str>,
    pub(crate) prefix: bool,
    pub(crate) protected: bool,
    pub(crate) reverse: bool,
    /// The base URL of the context that defined the term (for its scoped context).
    pub(crate) base_url: Option<Arc<Iri<String>>>,
    /// The scoped context.
    pub(crate) context: Option<Arc<Value<'static>>>,
    pub(crate) container: Container,
    /// `Some(None)`: explicitly no direction.
    pub(crate) direction: Option<Option<Direction>>,
    pub(crate) index: Option<Str>,
    /// `Some(None)`: explicitly no language.
    pub(crate) language: Option<Option<Str>>,
    pub(crate) nest: Option<Str>,
    /// `@id`, `@vocab`, `@json`, `@none` or a datatype IRI.
    pub(crate) type_mapping: Option<Str>,
}

impl TermDefinition {
    /// The same definition but for `protected` (§4.2 step 29.1).
    fn same_as(&self, other: &Self) -> bool {
        self.iri == other.iri
            && self.prefix == other.prefix
            && self.reverse == other.reverse
            && self.context == other.context
            && self.container == other.container
            && self.direction == other.direction
            && self.index == other.index
            && self.language == other.language
            && self.nest == other.nest
            && self.type_mapping == other.type_mapping
    }
}

/// An active context.
#[derive(Debug, Clone, Default)]
pub(crate) struct Context {
    terms: Arc<Map<Str, Arc<TermDefinition>>>,
    pub(crate) base: Option<Arc<Iri<String>>>,
    pub(crate) original_base: Option<Arc<Iri<String>>>,
    pub(crate) vocab: Option<Str>,
    pub(crate) language: Option<Str>,
    pub(crate) direction: Option<Direction>,
    /// The context to go back to in a new node object, after a non-propagated one.
    pub(crate) previous: Option<Arc<Context>>,
}

/// An expanded IRI: shared from a term definition, borrowed from the input, or new.
#[derive(Debug, Clone)]
pub(crate) enum Expanded<'v> {
    Shared(Str),
    Borrowed(&'v str),
    Owned(String),
}

impl Expanded<'_> {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Shared(s) => s,
            Self::Borrowed(s) => s,
            Self::Owned(s) => s,
        }
    }

    pub(crate) fn into_shared(self) -> Str {
        match self {
            Self::Shared(s) => s,
            Self::Borrowed(s) => Str::from(s),
            Self::Owned(s) => Str::from(s),
        }
    }
}

impl Context {
    pub(crate) fn term(&self, term: &str) -> Option<&Arc<TermDefinition>> {
        self.terms.get(term)
    }

    fn has_protected_terms(&self) -> bool {
        self.terms.values().any(|d| d.protected)
    }

    /// IRI Expansion (§5.2) outside context processing.
    pub(crate) fn expand_iri<'v>(
        &self,
        value: &'v str,
        document_relative: bool,
        vocab: bool,
    ) -> Option<Expanded<'v>> {
        if is_keyword(value) {
            return Some(Expanded::Borrowed(value));
        }
        if has_keyword_form(value) {
            return None;
        }
        if let Some(definition) = self.terms.get(value) {
            if let Some(iri) = &definition.iri
                && is_keyword(iri)
            {
                return Some(Expanded::Shared(iri.clone()));
            }
            if vocab {
                return definition.iri.clone().map(Expanded::Shared);
            }
        }
        if let Some(colon) = prefix_colon(value) {
            let (prefix, suffix) = (&value[..colon], &value[colon + 1..]);
            if prefix == "_" || suffix.starts_with("//") {
                return Some(Expanded::Borrowed(value));
            }
            if let Some(definition) = self.terms.get(prefix)
                && let Some(iri) = &definition.iri
                && definition.prefix
            {
                return Some(Expanded::Owned(format!("{iri}{suffix}")));
            }
            if has_iri_form(value) {
                return Some(Expanded::Borrowed(value));
            }
        }
        if vocab && let Some(mapping) = &self.vocab {
            return Some(Expanded::Owned(format!("{mapping}{value}")));
        }
        if document_relative
            && let Some(base) = &self.base
            && let Ok(iri) = base.resolve(value)
        {
            return Some(Expanded::Owned(iri.into_inner()));
        }
        Some(Expanded::Borrowed(value))
    }

    /// What `key` means as a key: a keyword, or a property IRI.
    pub(crate) fn expand_key<'v>(&self, key: &'v str) -> Option<Expanded<'v>> {
        self.expand_iri(key, false, true)
    }
}

/// A local context's entries, with an index when there are many.
struct Local<'c, 'v> {
    object: &'c Object<'v>,
    index: Option<HashMap<&'c str, &'c Value<'v>>>,
}

impl<'c, 'v> Local<'c, 'v> {
    fn new(object: &'c Object<'v>) -> Self {
        let index = (object.len() > 16).then(|| object.iter().collect());
        Self { object, index }
    }

    fn get(&self, key: &str) -> Option<&'c Value<'v>> {
        match &self.index {
            Some(index) => index.get(key).copied(),
            None => self.object.get(key),
        }
    }
}

/// The arguments Create Term Definition passes along.
struct TermArgs<'a> {
    base_url: Option<&'a Arc<Iri<String>>>,
    protected: bool,
    override_protected: bool,
    remote: &'a [String],
    validate_scoped: bool,
}

/// A remote context: its `@context` value and where it was read from.
struct Remote {
    context: Value<'static>,
    url: Arc<Iri<String>>,
}

/// What processing one document needs beyond its contexts: the options, the remote
/// contexts read so far, and the memo of scoped contexts.
pub(crate) struct Processor {
    pub(crate) options: JsonLdOptions,
    remote: Map<String, Arc<Remote>>,
    #[expect(clippy::type_complexity)]
    memo: Map<(usize, usize, u8), (Arc<Context>, Arc<Context>, Arc<TermDefinition>)>,
}

impl Processor {
    pub(crate) fn new(options: JsonLdOptions) -> Self {
        Self {
            options,
            remote: Map::default(),
            memo: Map::default(),
        }
    }

    pub(crate) fn is_1_0(&self) -> bool {
        self.options.processing_mode == JsonLdProcessingMode::JsonLd10
    }

    /// The initial active context for a document at `base`, with the expand context if
    /// the options have one.
    pub(crate) fn initial_context(
        &mut self,
        base: Option<Arc<Iri<String>>>,
    ) -> Result<Arc<Context>, JsonLdError> {
        let initial = Context {
            base: base.clone(),
            original_base: base.clone(),
            ..Context::default()
        };
        match self.options.expand_context.clone() {
            Some(context) => Ok(Arc::new(self.process(
                &initial,
                &context,
                base.as_ref(),
                &mut Vec::new(),
                false,
                true,
                true,
            )?)),
            None => Ok(Arc::new(initial)),
        }
    }

    /// `active` with the scoped context of `definition` applied, memoised.
    pub(crate) fn scoped(
        &mut self,
        active: &Arc<Context>,
        definition: &Arc<TermDefinition>,
        override_protected: bool,
        propagate: bool,
    ) -> Result<Arc<Context>, JsonLdError> {
        let Some(local) = &definition.context else {
            return Ok(active.clone());
        };
        let key = (
            Arc::as_ptr(active) as usize,
            Arc::as_ptr(definition) as usize,
            u8::from(override_protected) | (u8::from(propagate) << 1),
        );
        if let Some((context, _, _)) = self.memo.get(&key) {
            return Ok(context.clone());
        }
        let context = Arc::new(self.process(
            active,
            local,
            definition.base_url.as_ref(),
            &mut Vec::new(),
            override_protected,
            propagate,
            true,
        )?);
        if self.memo.len() >= MEMO_SIZE {
            self.memo.clear();
        }
        // The memo holds both inputs, so their addresses can't be reused while it does.
        self.memo
            .insert(key, (context.clone(), active.clone(), definition.clone()));
        Ok(context)
    }

    /// The Context Processing algorithm (§4.1.2).
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn process(
        &mut self,
        active: &Context,
        local: &Value<'_>,
        base_url: Option<&Arc<Iri<String>>>,
        remote: &mut Vec<String>,
        override_protected: bool,
        mut propagate: bool,
        validate_scoped: bool,
    ) -> Result<Context, JsonLdError> {
        let mut result = active.clone();
        if let Value::Object(object) = local
            && let Some(value) = object.get("@propagate")
        {
            propagate = value.as_bool().ok_or_else(|| {
                error(
                    Code::InvalidPropagateValue,
                    "@propagate must be true or false",
                )
            })?;
        }
        if !propagate && result.previous.is_none() {
            result.previous = Some(Arc::new(active.clone()));
        }
        for context in local.as_items() {
            match context {
                Value::Null => {
                    if !override_protected && result.has_protected_terms() {
                        return Err(error(
                            Code::InvalidContextNullification,
                            "a context with protected terms can't be nulled",
                        ));
                    }
                    let previous = result;
                    result = Context {
                        base: active.original_base.clone(),
                        original_base: active.original_base.clone(),
                        ..Context::default()
                    };
                    if !propagate {
                        result.previous = Some(Arc::new(previous));
                    }
                }
                Value::String(reference) => {
                    let url = resolve_url(base_url, reference).ok_or_else(|| {
                        error(
                            Code::LoadingDocumentFailed,
                            format!("the context IRI {reference:?} is not valid"),
                        )
                    })?;
                    if !validate_scoped && remote.contains(&url) {
                        continue;
                    }
                    if remote.len() >= MAX_REMOTE_CONTEXTS {
                        return Err(error(
                            Code::ContextOverflow,
                            format!("more than {MAX_REMOTE_CONTEXTS} nested remote contexts"),
                        ));
                    }
                    remote.push(url.clone());
                    let loaded = self.load(&url)?;
                    let mut nested = remote.clone();
                    result = self.process(
                        &result,
                        &loaded.context,
                        Some(&loaded.url),
                        &mut nested,
                        false,
                        true,
                        validate_scoped,
                    )?;
                }
                Value::Object(definition) => {
                    self.definition(
                        &mut result,
                        definition,
                        base_url,
                        remote,
                        override_protected,
                        validate_scoped,
                    )?;
                }
                _ => {
                    return Err(error(
                        Code::InvalidLocalContext,
                        format!("a context must be an object, a string or null, not {context}"),
                    ));
                }
            }
        }
        Ok(result)
    }

    /// §4.1.2 step 5.5 on: a context definition (an object).
    fn definition(
        &mut self,
        result: &mut Context,
        definition: &Object<'_>,
        base_url: Option<&Arc<Iri<String>>>,
        remote: &[String],
        override_protected: bool,
        validate_scoped: bool,
    ) -> Result<(), JsonLdError> {
        if let Some(version) = definition.get("@version") {
            if !matches!(version, Value::Number(n) if n.parse::<f64>().ok() == Some(1.1)) {
                return Err(error(
                    Code::InvalidVersionValue,
                    format!("@version {version} isn't 1.1"),
                ));
            }
            if self.is_1_0() {
                return Err(error(
                    Code::ProcessingModeConflict,
                    "@version 1.1 in JSON-LD 1.0 mode",
                ));
            }
        }
        // @import: the imported context's entries, overridden by this one's.
        let merged;
        let definition = match definition.get("@import") {
            None => definition,
            Some(import) => {
                if self.is_1_0() {
                    return Err(error(
                        Code::InvalidContextEntry,
                        "@import in JSON-LD 1.0 mode",
                    ));
                }
                let Value::String(reference) = import else {
                    return Err(error(Code::InvalidImportValue, "@import must be a string"));
                };
                let url = resolve_url(base_url, reference).ok_or_else(|| {
                    error(
                        Code::LoadingDocumentFailed,
                        format!("the @import IRI {reference:?} is not valid"),
                    )
                })?;
                let loaded = self.load(&url)?;
                let Value::Object(imported) = &loaded.context else {
                    return Err(error(
                        Code::InvalidRemoteContext,
                        format!("{url} has no context object to import"),
                    ));
                };
                if imported.contains_key("@import") {
                    return Err(error(
                        Code::InvalidContextEntry,
                        format!("the imported {url} imports too"),
                    ));
                }
                let mut object: Object<'_> = imported.clone();
                for (key, value) in definition.iter() {
                    object.insert(Cow::Owned(key.to_owned()), value.clone());
                }
                merged = object;
                &merged
            }
        };
        if let Some(base) = definition.get("@base")
            && remote.is_empty()
        {
            match base {
                Value::Null => result.base = None,
                Value::String(s) => {
                    let resolved = match Iri::parse(s.to_string()) {
                        Ok(iri) => iri,
                        // An absolute IRI that isn't well formed: IRIs resolved against it
                        // won't be either, and are dropped when they become RDF.
                        Err(_) if has_iri_form(s) => Iri::parse_unchecked(s.to_string()),
                        Err(_) => match &result.base {
                            Some(current) => current.resolve(s).map_err(|e| {
                                error(Code::InvalidBaseIri, format!("@base {s:?}: {e}"))
                            })?,
                            None => {
                                return Err(error(
                                    Code::InvalidBaseIri,
                                    format!("@base {s:?} is relative, and there is no base"),
                                ));
                            }
                        },
                    };
                    result.base = Some(Arc::new(resolved));
                }
                _ => {
                    return Err(error(
                        Code::InvalidBaseIri,
                        "@base must be a string or null",
                    ));
                }
            }
        }
        if let Some(vocab) = definition.get("@vocab") {
            match vocab {
                Value::Null => result.vocab = None,
                Value::String(s) => {
                    let expanded = result
                        .expand_iri(s, true, true)
                        .map(Expanded::into_shared)
                        .filter(|e| !self.is_1_0() || is_iri_or_blank(e))
                        .ok_or_else(|| {
                            error(
                                Code::InvalidVocabMapping,
                                format!("@vocab {s:?} is not an IRI"),
                            )
                        })?;
                    result.vocab = Some(expanded);
                }
                _ => {
                    return Err(error(
                        Code::InvalidVocabMapping,
                        "@vocab must be a string or null",
                    ));
                }
            }
        }
        if let Some(language) = definition.get("@language") {
            result.language = match language {
                Value::Null => None,
                Value::String(s) => Some(Str::from(s.as_ref())),
                _ => {
                    return Err(error(
                        Code::InvalidDefaultLanguage,
                        "@language must be a string or null",
                    ));
                }
            };
        }
        if let Some(direction) = definition.get("@direction") {
            if self.is_1_0() {
                return Err(error(
                    Code::InvalidContextEntry,
                    "@direction in JSON-LD 1.0 mode",
                ));
            }
            result.direction = match direction {
                Value::Null => None,
                Value::String(s) => Some(Direction::parse(s).ok_or_else(|| {
                    error(
                        Code::InvalidBaseDirection,
                        format!("@direction {s:?} isn't ltr or rtl"),
                    )
                })?),
                _ => {
                    return Err(error(
                        Code::InvalidBaseDirection,
                        "@direction must be a string or null",
                    ));
                }
            };
        }
        if let Some(propagate) = definition.get("@propagate") {
            if self.is_1_0() {
                return Err(error(
                    Code::InvalidContextEntry,
                    "@propagate in JSON-LD 1.0 mode",
                ));
            }
            if propagate.as_bool().is_none() {
                return Err(error(
                    Code::InvalidPropagateValue,
                    "@propagate must be true or false",
                ));
            }
        }
        let protected = match definition.get("@protected") {
            None => false,
            Some(Value::Boolean(b)) => *b,
            Some(_) => {
                return Err(error(
                    Code::InvalidProtectedValue,
                    "@protected must be true or false",
                ));
            }
        };
        let local = Local::new(definition);
        let mut defined: HashMap<String, bool> = HashMap::new();
        let args = TermArgs {
            base_url,
            protected,
            override_protected,
            remote,
            validate_scoped,
        };
        for key in definition.keys() {
            if !matches!(
                key,
                "@base"
                    | "@direction"
                    | "@import"
                    | "@language"
                    | "@propagate"
                    | "@protected"
                    | "@version"
                    | "@vocab"
            ) {
                self.create_term(result, &local, key, &mut defined, &args)?;
            }
        }
        Ok(())
    }

    /// IRI Expansion during context processing: terms of the local context it depends on
    /// are defined first (§5.2 steps 3 and 6.3).
    #[expect(clippy::too_many_arguments)]
    fn expand_iri_defining(
        &mut self,
        active: &mut Context,
        local: &Local<'_, '_>,
        defined: &mut HashMap<String, bool>,
        value: &str,
        document_relative: bool,
        vocab: bool,
        args: &TermArgs<'_>,
    ) -> Result<Option<String>, JsonLdError> {
        if is_keyword(value) {
            return Ok(Some(value.to_owned()));
        }
        if has_keyword_form(value) {
            return Ok(None);
        }
        if local.get(value).is_some() && defined.get(value) != Some(&true) {
            self.create_term(active, local, value, defined, args)?;
        }
        if let Some(definition) = active.term(value) {
            let keyword = definition.iri.as_deref().filter(|iri| is_keyword(iri));
            if keyword.is_some() || vocab {
                return Ok(definition.iri.as_deref().map(str::to_owned));
            }
        }
        if let Some(colon) = prefix_colon(value) {
            let (prefix, suffix) = (&value[..colon], &value[colon + 1..]);
            if prefix != "_"
                && !suffix.starts_with("//")
                && local.get(prefix).is_some()
                && defined.get(prefix) != Some(&true)
            {
                self.create_term(active, local, prefix, defined, args)?;
            }
        }
        Ok(active
            .expand_iri(value, document_relative, vocab)
            .map(|e| e.as_str().to_owned()))
    }

    /// The Create Term Definition algorithm (§4.2.2).
    fn create_term(
        &mut self,
        active: &mut Context,
        local: &Local<'_, '_>,
        term: &str,
        defined: &mut HashMap<String, bool>,
        args: &TermArgs<'_>,
    ) -> Result<(), JsonLdError> {
        match defined.get(term) {
            Some(true) => return Ok(()),
            Some(false) => {
                return Err(error(
                    Code::CyclicIriMapping,
                    format!("the term {term:?} depends on itself"),
                ));
            }
            None => {}
        }
        if term.is_empty() {
            return Err(error(Code::InvalidTermDefinition, "a term can't be empty"));
        }
        defined.insert(term.to_owned(), false);
        let value = local.get(term).cloned().unwrap_or_default();
        if term == "@type" {
            let valid = !self.is_1_0()
                && matches!(&value, Value::Object(o) if !o.is_empty()
                && o.iter().all(|(k, v)| match k {
                    "@container" => v.as_str() == Some("@set"),
                    "@protected" => true,
                    _ => false,
                }));
            if !valid {
                return Err(error(
                    Code::KeywordRedefinition,
                    "@type can only be given @container @set and @protected",
                ));
            }
        } else if is_keyword(term) {
            return Err(error(
                Code::KeywordRedefinition,
                format!("the keyword {term} can't be redefined"),
            ));
        } else if has_keyword_form(term) {
            defined.insert(term.to_owned(), true);
            return Ok(());
        }
        let previous = Arc::make_mut(&mut active.terms).remove(term);
        let (map, simple) = match value {
            Value::Null => (Object::from_iter([("@id", Value::Null)]), false),
            Value::String(s) => (Object::from_iter([("@id", Value::String(s))]), true),
            Value::Object(o) => (o, false),
            other => {
                return Err(error(
                    Code::InvalidTermDefinition,
                    format!("the definition of {term:?} is {other}"),
                ));
            }
        };
        let mut definition = TermDefinition {
            protected: args.protected,
            ..TermDefinition::default()
        };
        if let Some(protected) = map.get("@protected") {
            if self.is_1_0() {
                return Err(error(
                    Code::InvalidTermDefinition,
                    "@protected in JSON-LD 1.0 mode",
                ));
            }
            definition.protected = protected.as_bool().ok_or_else(|| {
                error(
                    Code::InvalidProtectedValue,
                    "@protected must be true or false",
                )
            })?;
        }
        if let Some(t) = map.get("@type") {
            let Value::String(t) = t else {
                return Err(error(
                    Code::InvalidTypeMapping,
                    format!("the @type of {term:?} must be a string"),
                ));
            };
            let t = self
                .expand_iri_defining(active, local, defined, t, false, true, args)?
                .unwrap_or_default();
            let valid = match t.as_str() {
                "@json" | "@none" => !self.is_1_0(),
                "@id" | "@vocab" => true,
                other => has_iri_form(other) && !other.starts_with("_:"),
            };
            if !valid {
                return Err(error(
                    Code::InvalidTypeMapping,
                    format!("the @type of {term:?} is {t:?}"),
                ));
            }
            definition.type_mapping = Some(Str::from(t));
        }
        if let Some(reverse) = map.get("@reverse") {
            if map.contains_key("@id") || map.contains_key("@nest") {
                return Err(error(
                    Code::InvalidReverseProperty,
                    format!("the reverse property {term:?} has @id or @nest"),
                ));
            }
            let Value::String(reverse) = reverse else {
                return Err(error(
                    Code::InvalidIriMapping,
                    format!("the @reverse of {term:?} must be a string"),
                ));
            };
            if has_keyword_form(reverse) {
                defined.insert(term.to_owned(), true);
                return Ok(());
            }
            let iri = self
                .expand_iri_defining(active, local, defined, reverse, false, true, args)?
                .filter(|iri| is_iri_or_blank(iri))
                .ok_or_else(|| {
                    error(
                        Code::InvalidIriMapping,
                        format!("the @reverse of {term:?} is not an IRI"),
                    )
                })?;
            definition.iri = Some(Str::from(iri));
            if let Some(container) = map.get("@container") {
                definition.container = match container {
                    Value::Null => Container::default(),
                    Value::String(s) if s == "@set" => Container(Container::SET),
                    Value::String(s) if s == "@index" => Container(Container::INDEX),
                    _ => {
                        return Err(error(
                            Code::InvalidReverseProperty,
                            format!(
                                "the container of the reverse property {term:?} must be @set or @index"
                            ),
                        ));
                    }
                };
            }
            definition.reverse = true;
        } else if let Some(id) = map.get("@id")
            && id.as_str() != Some(term)
        {
            match id {
                Value::Null => {}
                Value::String(id) => {
                    if !is_keyword(id) && has_keyword_form(id) {
                        defined.insert(term.to_owned(), true);
                        return Ok(());
                    }
                    let iri = self
                        .expand_iri_defining(active, local, defined, id, false, true, args)?
                        .filter(|iri| is_keyword(iri) || is_iri_or_blank(iri))
                        .ok_or_else(|| {
                            error(
                                Code::InvalidIriMapping,
                                format!("the @id of {term:?} is not an IRI"),
                            )
                        })?;
                    if iri == "@context" {
                        return Err(error(
                            Code::InvalidKeywordAlias,
                            "@context can't be aliased",
                        ));
                    }
                    let inner_colon = term
                        .char_indices()
                        .any(|(i, c)| c == ':' && i > 0 && i + 1 < term.len());
                    if inner_colon || term.contains('/') {
                        defined.insert(term.to_owned(), true);
                        let own = self
                            .expand_iri_defining(active, local, defined, term, false, true, args)?;
                        if own.as_deref() != Some(iri.as_str()) {
                            return Err(error(
                                Code::InvalidIriMapping,
                                format!("the term {term:?} looks like another IRI than its @id"),
                            ));
                        }
                    }
                    if !term.contains(':')
                        && !term.contains('/')
                        && simple
                        && (iri.starts_with("_:")
                            || iri.ends_with([':', '/', '?', '#', '[', ']', '@']))
                    {
                        definition.prefix = true;
                    }
                    definition.iri = Some(Str::from(iri));
                }
                _ => {
                    return Err(error(
                        Code::InvalidIriMapping,
                        format!("the @id of {term:?} must be a string"),
                    ));
                }
            }
        } else if let Some((colon, _)) = term.char_indices().skip(1).find(|&(_, c)| c == ':') {
            let (prefix, suffix) = (&term[..colon], &term[colon + 1..]);
            if local.get(prefix).is_some() {
                self.create_term(active, local, prefix, defined, args)?;
            }
            definition.iri = Some(match active.term(prefix).and_then(|d| d.iri.as_ref()) {
                Some(prefix_iri) => Str::from(format!("{prefix_iri}{suffix}")),
                None => Str::from(term),
            });
        } else if term.contains('/') {
            // A relative IRI, expanded without the local context (§4.2 step 16.1).
            let iri = active
                .expand_iri(term, false, true)
                .map(|e| e.as_str().to_owned())
                .filter(|iri| has_iri_form(iri))
                .ok_or_else(|| {
                    error(
                        Code::InvalidIriMapping,
                        format!("the term {term:?} is not an IRI"),
                    )
                })?;
            definition.iri = Some(Str::from(iri));
        } else if term == "@type" {
            definition.iri = Some(Str::from("@type"));
        } else if let Some(vocab) = &active.vocab {
            definition.iri = Some(Str::from(format!("{vocab}{term}")));
        } else {
            return Err(error(
                Code::InvalidIriMapping,
                format!("the term {term:?} has no IRI and there is no @vocab"),
            ));
        }
        if let Some(container) = map.get("@container")
            && !definition.reverse
        {
            let mut flags = 0;
            let mut strings = 0;
            for item in container.as_items() {
                let flag = item.as_str().and_then(Container::flag).ok_or_else(|| {
                    error(
                        Code::InvalidContainerMapping,
                        format!("the container of {term:?} is {container}"),
                    )
                })?;
                flags |= flag;
                strings += 1;
            }
            let container_mapping = Container(flags);
            if !container_mapping.is_valid() || strings == 0 {
                return Err(error(
                    Code::InvalidContainerMapping,
                    format!("the container of {term:?} is {container}"),
                ));
            }
            if self.is_1_0()
                && (!matches!(container, Value::String(_))
                    || container_mapping.has(Container::GRAPH | Container::ID | Container::TYPE))
            {
                return Err(error(
                    Code::InvalidContainerMapping,
                    format!("the container of {term:?} needs JSON-LD 1.1"),
                ));
            }
            definition.container = container_mapping;
            if container_mapping.has(Container::TYPE) {
                match definition.type_mapping.as_deref() {
                    None => definition.type_mapping = Some(Str::from("@id")),
                    Some("@id" | "@vocab") => {}
                    Some(_) => {
                        return Err(error(
                            Code::InvalidTypeMapping,
                            format!("a type map {term:?} needs @type @id or @vocab"),
                        ));
                    }
                }
            }
        }
        if let Some(index) = map.get("@index") {
            if self.is_1_0() || !definition.container.has(Container::INDEX) {
                return Err(error(
                    Code::InvalidTermDefinition,
                    format!("{term:?} has @index but no index container"),
                ));
            }
            let Value::String(index) = index else {
                return Err(error(
                    Code::InvalidTermDefinition,
                    format!("the @index of {term:?} must be a string"),
                ));
            };
            let expanded =
                self.expand_iri_defining(active, local, defined, index, false, true, args)?;
            if !expanded.is_some_and(|e| has_iri_form(&e) && !is_keyword(&e)) {
                return Err(error(
                    Code::InvalidTermDefinition,
                    format!("the @index of {term:?} is not an IRI"),
                ));
            }
            definition.index = Some(Str::from(index.as_ref()));
        }
        if let Some(context) = map.get("@context") {
            if self.is_1_0() {
                return Err(error(
                    Code::InvalidTermDefinition,
                    "a scoped context in JSON-LD 1.0 mode",
                ));
            }
            // Checked now for errors (and checked once: the check doesn't check the scoped
            // contexts it meets in turn, which may be recursive).
            if args.validate_scoped {
                let mut remote = args.remote.to_vec();
                self.process(
                    active,
                    context,
                    args.base_url,
                    &mut remote,
                    true,
                    true,
                    false,
                )
                .map_err(|e| {
                    error(
                        Code::InvalidScopedContext,
                        format!("the context of {term:?}: {e}"),
                    )
                })?;
            }
            definition.context = Some(Arc::new(context.clone().into_owned()));
            definition.base_url = args.base_url.cloned();
        }
        if !map.contains_key("@type") {
            if let Some(language) = map.get("@language") {
                definition.language = Some(match language {
                    Value::Null => None,
                    Value::String(s) => Some(Str::from(s.as_ref())),
                    _ => {
                        return Err(error(
                            Code::InvalidLanguageMapping,
                            format!("the @language of {term:?} must be a string or null"),
                        ));
                    }
                });
            }
            if let Some(direction) = map.get("@direction") {
                definition.direction = Some(match direction {
                    Value::Null => None,
                    Value::String(s) => Some(Direction::parse(s).ok_or_else(|| {
                        error(
                            Code::InvalidBaseDirection,
                            format!("the @direction of {term:?} isn't ltr or rtl"),
                        )
                    })?),
                    _ => {
                        return Err(error(
                            Code::InvalidBaseDirection,
                            format!("the @direction of {term:?} must be a string or null"),
                        ));
                    }
                });
            }
        }
        if let Some(nest) = map.get("@nest") {
            if self.is_1_0() {
                return Err(error(
                    Code::InvalidTermDefinition,
                    "@nest in JSON-LD 1.0 mode",
                ));
            }
            match nest {
                Value::String(s) if !is_keyword(s) || s == "@nest" => {
                    definition.nest = Some(Str::from(s.as_ref()));
                }
                _ => {
                    return Err(error(
                        Code::InvalidNestValue,
                        format!("the @nest of {term:?} is {nest}"),
                    ));
                }
            }
        }
        if let Some(prefix) = map.get("@prefix") {
            if self.is_1_0() || term.contains(':') || term.contains('/') {
                return Err(error(
                    Code::InvalidTermDefinition,
                    format!("{term:?} can't have @prefix"),
                ));
            }
            definition.prefix = prefix.as_bool().ok_or_else(|| {
                error(
                    Code::InvalidPrefixValue,
                    format!("the @prefix of {term:?} must be true or false"),
                )
            })?;
            if definition.prefix && definition.iri.as_deref().is_some_and(is_keyword) {
                return Err(error(
                    Code::InvalidTermDefinition,
                    format!("the keyword alias {term:?} can't be a prefix"),
                ));
            }
        }
        if let Some(unknown) = map.keys().find(|k| {
            !matches!(
                *k,
                "@id"
                    | "@reverse"
                    | "@container"
                    | "@context"
                    | "@direction"
                    | "@index"
                    | "@language"
                    | "@nest"
                    | "@prefix"
                    | "@protected"
                    | "@type"
            )
        }) {
            return Err(error(
                Code::InvalidTermDefinition,
                format!("the definition of {term:?} has the entry {unknown:?}"),
            ));
        }
        let definition = match previous {
            Some(previous) if !args.override_protected && previous.protected => {
                if !definition.same_as(&previous) {
                    return Err(error(
                        Code::ProtectedTermRedefinition,
                        format!("the protected term {term:?} can't be redefined"),
                    ));
                }
                previous
            }
            _ => Arc::new(definition),
        };
        Arc::make_mut(&mut active.terms).insert(Str::from(term), definition);
        defined.insert(term.to_owned(), true);
        Ok(())
    }

    /// A remote context, read once per document.
    fn load(&mut self, url: &str) -> Result<Arc<Remote>, JsonLdError> {
        if let Some(remote) = self.remote.get(url) {
            return Ok(remote.clone());
        }
        let failed = |why: String| error(Code::LoadingRemoteContextFailed, format!("{url}: {why}"));
        let Some(loader) = &self.options.loader else {
            return Err(failed(
                "reading remote contexts is off (no document loader)".into(),
            ));
        };
        let document = loader(url).map_err(failed)?;
        let value = Value::parse(&document.document)
            .map_err(|e| failed(e.to_string()))?
            .into_owned();
        let Value::Object(mut object) = value else {
            return Err(error(
                Code::InvalidRemoteContext,
                format!("{url} is not a JSON object"),
            ));
        };
        let context = object
            .remove("@context")
            .ok_or_else(|| error(Code::InvalidRemoteContext, format!("{url} has no @context")))?;
        let url_iri = Iri::parse(document.document_url.clone())
            .or_else(|_| Iri::parse(url.to_owned()))
            .map_err(|e| failed(e.to_string()))?;
        let remote = Arc::new(Remote {
            context,
            url: Arc::new(url_iri),
        });
        self.remote.insert(url.to_owned(), remote.clone());
        Ok(remote)
    }
}

/// A context reference resolved against the base URL (or as it is, if absolute).
fn resolve_url(base: Option<&Arc<Iri<String>>>, reference: &str) -> Option<String> {
    match base {
        Some(base) => base.resolve(reference).ok().map(Iri::into_inner),
        None => Iri::parse(reference.to_owned()).ok().map(Iri::into_inner),
    }
}
