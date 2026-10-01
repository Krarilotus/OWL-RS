//! JSON-LD 1.1: to RDF (context processing, expansion, the RDF conversion) and from RDF.
//!
//! The algorithms are those of JSON-LD 1.1 Processing Algorithms and API, with two
//! changes of representation that leave the results alone:
//! - expansion produces typed node, value and list objects ([`items`]), not JSON maps,
//!   and the conversion to RDF walks them directly, without the node map (its triples are
//!   the same set);
//! - a top-level array, and the `@graph` of a top-level object whose only other entry is
//!   `@context`, are processed one element at a time ([`parser`]).
//!
//! Remote contexts and `@import` are read only through a [`DocumentLoader`] the caller
//! supplies: by default there is none, and a document that names one is an error.

mod context;
mod expand;
mod from_rdf;
mod items;
pub(crate) mod parser;
mod to_rdf;
pub(crate) mod writer;

use std::fmt;
use std::sync::Arc;

use nrese_json::Value;
use nrese_rdf::{Iri, Quad};

pub use from_rdf::{FromRdfOptions, from_rdf};

/// Reads a remote document for JSON-LD: the IRI asked for in, the document out (or why
/// not, as text).
pub type DocumentLoader = Arc<dyn Fn(&str) -> Result<RemoteDocument, String> + Send + Sync>;

/// A document a [`DocumentLoader`] read.
#[derive(Debug, Clone)]
pub struct RemoteDocument {
    /// The IRI it was read from in the end (after redirects); relative IRIs in it resolve
    /// against this.
    pub document_url: String,
    /// The JSON text.
    pub document: String,
}

/// Which version of JSON-LD's rules apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JsonLdProcessingMode {
    /// JSON-LD 1.0: the 1.1 features are errors.
    JsonLd10,
    #[default]
    JsonLd11,
}

/// How a literal's base direction (`@direction`) becomes RDF, and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdfDirection {
    /// A datatype `https://www.w3.org/ns/i18n#{language}_{direction}`.
    I18nDatatype,
    /// A blank node with `rdf:value`, `rdf:language` and `rdf:direction`.
    CompoundLiteral,
}

/// The JSON-LD settings of a parser.
#[derive(Clone, Default)]
pub struct JsonLdOptions {
    pub(crate) processing_mode: JsonLdProcessingMode,
    pub(crate) rdf_direction: Option<RdfDirection>,
    pub(crate) expand_context: Option<Arc<Value<'static>>>,
    pub(crate) loader: Option<DocumentLoader>,
}

impl fmt::Debug for JsonLdOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JsonLdOptions")
            .field("processing_mode", &self.processing_mode)
            .field("rdf_direction", &self.rdf_direction)
            .field("expand_context", &self.expand_context)
            .field("loader", &self.loader.is_some())
            .finish()
    }
}

impl JsonLdOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_processing_mode(mut self, mode: JsonLdProcessingMode) -> Self {
        self.processing_mode = mode;
        self
    }

    /// Literals with a base direction become RDF this way (by default the direction is
    /// dropped, as RDF 1.1 has no place for it).
    pub fn with_rdf_direction(mut self, direction: RdfDirection) -> Self {
        self.rdf_direction = Some(direction);
        self
    }

    /// A context applied before the document's own (the API's `expandContext`): a JSON
    /// context, or an object with an `@context` entry.
    pub fn with_expand_context(mut self, context: &str) -> Result<Self, JsonLdError> {
        let value = Value::parse(context)
            .map_err(|e| JsonLdError::new(JsonLdErrorCode::InvalidLocalContext, e.to_string()))?
            .into_owned();
        let value = match value {
            Value::Object(mut object) if object.contains_key("@context") => {
                object.remove("@context").unwrap_or_default()
            }
            other => other,
        };
        self.expand_context = Some(Arc::new(value));
        Ok(self)
    }

    /// Remote contexts and `@import`s are read with `loader` (none by default: a
    /// document that names one is an error, so parsing never reaches the network unless
    /// the caller says how).
    pub fn with_document_loader(
        mut self,
        loader: impl Fn(&str) -> Result<RemoteDocument, String> + Send + Sync + 'static,
    ) -> Self {
        self.loader = Some(Arc::new(loader));
        self
    }
}

/// The error codes of JSON-LD 1.1 API §9.4.2 that processing to and from RDF can raise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonLdErrorCode {
    CollidingKeywords,
    ContextOverflow,
    CyclicIriMapping,
    InvalidBaseDirection,
    InvalidBaseIri,
    InvalidContainerMapping,
    InvalidContextEntry,
    InvalidContextNullification,
    InvalidDefaultLanguage,
    InvalidIdValue,
    InvalidImportValue,
    InvalidIncludedValue,
    InvalidIndexValue,
    InvalidIriMapping,
    InvalidJsonLiteral,
    InvalidKeywordAlias,
    InvalidLanguageMapValue,
    InvalidLanguageMapping,
    InvalidLanguageTaggedString,
    InvalidLanguageTaggedValue,
    InvalidLocalContext,
    InvalidNestValue,
    InvalidPrefixValue,
    InvalidPropagateValue,
    InvalidProtectedValue,
    InvalidRemoteContext,
    InvalidReverseProperty,
    InvalidReversePropertyMap,
    InvalidReversePropertyValue,
    InvalidReverseValue,
    InvalidScopedContext,
    InvalidSetOrListObject,
    InvalidTermDefinition,
    InvalidTypeMapping,
    InvalidTypeValue,
    InvalidTypedValue,
    InvalidValueObject,
    InvalidValueObjectValue,
    InvalidVersionValue,
    InvalidVocabMapping,
    KeywordRedefinition,
    ListOfLists,
    LoadingDocumentFailed,
    LoadingRemoteContextFailed,
    ProcessingModeConflict,
    ProtectedTermRedefinition,
}

impl JsonLdErrorCode {
    /// The code as the specification writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CollidingKeywords => "colliding keywords",
            Self::ContextOverflow => "context overflow",
            Self::CyclicIriMapping => "cyclic IRI mapping",
            Self::InvalidBaseDirection => "invalid base direction",
            Self::InvalidBaseIri => "invalid base IRI",
            Self::InvalidContainerMapping => "invalid container mapping",
            Self::InvalidContextEntry => "invalid context entry",
            Self::InvalidContextNullification => "invalid context nullification",
            Self::InvalidDefaultLanguage => "invalid default language",
            Self::InvalidIdValue => "invalid @id value",
            Self::InvalidImportValue => "invalid @import value",
            Self::InvalidIncludedValue => "invalid @included value",
            Self::InvalidIndexValue => "invalid @index value",
            Self::InvalidIriMapping => "invalid IRI mapping",
            Self::InvalidJsonLiteral => "invalid JSON literal",
            Self::InvalidKeywordAlias => "invalid keyword alias",
            Self::InvalidLanguageMapValue => "invalid language map value",
            Self::InvalidLanguageMapping => "invalid language mapping",
            Self::InvalidLanguageTaggedString => "invalid language-tagged string",
            Self::InvalidLanguageTaggedValue => "invalid language-tagged value",
            Self::InvalidLocalContext => "invalid local context",
            Self::InvalidNestValue => "invalid @nest value",
            Self::InvalidPrefixValue => "invalid @prefix value",
            Self::InvalidPropagateValue => "invalid @propagate value",
            Self::InvalidProtectedValue => "invalid @protected value",
            Self::InvalidRemoteContext => "invalid remote context",
            Self::InvalidReverseProperty => "invalid reverse property",
            Self::InvalidReversePropertyMap => "invalid reverse property map",
            Self::InvalidReversePropertyValue => "invalid reverse property value",
            Self::InvalidReverseValue => "invalid @reverse value",
            Self::InvalidScopedContext => "invalid scoped context",
            Self::InvalidSetOrListObject => "invalid set or list object",
            Self::InvalidTermDefinition => "invalid term definition",
            Self::InvalidTypeMapping => "invalid type mapping",
            Self::InvalidTypeValue => "invalid type value",
            Self::InvalidTypedValue => "invalid typed value",
            Self::InvalidValueObject => "invalid value object",
            Self::InvalidValueObjectValue => "invalid value object value",
            Self::InvalidVersionValue => "invalid @version value",
            Self::InvalidVocabMapping => "invalid vocab mapping",
            Self::KeywordRedefinition => "keyword redefinition",
            Self::ListOfLists => "list of lists",
            Self::LoadingDocumentFailed => "loading document failed",
            Self::LoadingRemoteContextFailed => "loading remote context failed",
            Self::ProcessingModeConflict => "processing mode conflict",
            Self::ProtectedTermRedefinition => "protected term redefinition",
        }
    }
}

impl fmt::Display for JsonLdErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A JSON-LD processing error: its code and what in the document raised it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct JsonLdError {
    code: JsonLdErrorCode,
    message: String,
}

impl JsonLdError {
    pub(crate) fn new(code: JsonLdErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> JsonLdErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// The expanded form of the JSON-LD document `text` (the API's `expand`), as JSON: what
/// the conversion to RDF reads. `base` is the document's IRI.
pub fn expand(
    text: &str,
    base: Option<&str>,
    options: &JsonLdOptions,
) -> Result<Value<'static>, JsonLdError> {
    let document = Value::parse(text)
        .map_err(|e| JsonLdError::new(JsonLdErrorCode::LoadingDocumentFailed, e.to_string()))?;
    let base = base
        .map(|b| Iri::parse(b.to_owned()))
        .transpose()
        .map_err(|e| JsonLdError::new(JsonLdErrorCode::InvalidBaseIri, e.to_string()))?;
    let mut processor = context::Processor::new(options.clone());
    let initial = processor.initial_context(base.map(Arc::new))?;
    let items = expand::Expander::new(&mut processor).expand_document(&initial, &document)?;
    Ok(items::to_json(&items))
}

/// The quads of the JSON-LD document `text`, all at once (the API's `toRdf`); a
/// [`crate::RdfParser`] streams them instead.
pub fn to_rdf(
    text: &str,
    base: Option<&str>,
    options: &JsonLdOptions,
) -> Result<Vec<Quad>, crate::RdfParseError> {
    let mut parser = crate::RdfParser::from_format(crate::RdfFormat::JsonLd)
        .with_json_ld_options(options.clone());
    if let Some(base) = base {
        parser = parser.with_base_iri(base).map_err(|e| {
            crate::RdfSyntaxError::new(
                format!("{}: {e}", JsonLdErrorCode::InvalidBaseIri),
                Default::default(),
            )
        })?;
    }
    parser.for_slice(text.as_bytes()).collect()
}
