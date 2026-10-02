//! The Expansion algorithm (JSON-LD 1.1 API §5.1) and Value Expansion (§5.3), producing
//! the typed expanded form of [`super::items`].

use std::sync::Arc;

use nrese_json::{Object, Value};
use nrese_rdf::Iri;

use super::JsonLdError;
use super::JsonLdErrorCode as Code;
use super::context::{
    Container, Context, Direction, Expanded as Iri_, Processor, Str, error, has_iri_form,
    is_keyword,
};
use super::items::{Item, ListObject, Node, ValueObject, add};

/// What expanding an element gives: nothing, one item, or several (an array).
pub(crate) enum Expanded {
    Null,
    One(Item),
    Many(Vec<Item>),
}

impl Expanded {
    pub(crate) fn into_vec(self) -> Vec<Item> {
        match self {
            Self::Null => Vec::new(),
            Self::One(item) => vec![item],
            Self::Many(items) => items,
        }
    }
}

/// The keyword entries a result map has, for the checks of §5.1.2 steps 13.4.2 and 15–20.
mod key {
    pub(super) const ID: u16 = 1;
    pub(super) const TYPE: u16 = 1 << 1;
    pub(super) const VALUE: u16 = 1 << 2;
    pub(super) const LANGUAGE: u16 = 1 << 3;
    pub(super) const DIRECTION: u16 = 1 << 4;
    pub(super) const INDEX: u16 = 1 << 5;
    pub(super) const LIST: u16 = 1 << 6;
    pub(super) const SET: u16 = 1 << 7;
    pub(super) const REVERSE: u16 = 1 << 8;
    pub(super) const GRAPH: u16 = 1 << 9;
    pub(super) const INCLUDED: u16 = 1 << 10;

    pub(super) fn of(keyword: &str) -> u16 {
        match keyword {
            "@id" => ID,
            "@type" => TYPE,
            "@value" => VALUE,
            "@language" => LANGUAGE,
            "@direction" => DIRECTION,
            "@index" => INDEX,
            "@list" => LIST,
            "@set" => SET,
            "@reverse" => REVERSE,
            "@graph" => GRAPH,
            "@included" => INCLUDED,
            _ => 0,
        }
    }
}

/// The result map of one element, as it is built.
#[derive(Default)]
struct Builder {
    keys: u16,
    id: Option<Str>,
    id_null: bool,
    types: Vec<Str>,
    /// `@type` was an array (or given twice): a value object can't have that.
    types_array: bool,
    value: Option<Value<'static>>,
    language: Option<Str>,
    direction: Option<Direction>,
    index: Option<Str>,
    list: Option<Vec<Item>>,
    set: Option<Expanded>,
    reverse: Vec<(Str, Vec<Item>)>,
    graph: Option<Vec<Item>>,
    included: Option<Vec<Item>>,
    properties: Vec<(Str, Vec<Item>)>,
}

/// Expands the elements of one document.
pub(crate) struct Expander<'p> {
    processor: &'p mut Processor,
    /// The document's IRI: relative context references resolve against it.
    base_url: Option<Arc<Iri<String>>>,
}

impl<'p> Expander<'p> {
    pub(crate) fn new(processor: &'p mut Processor) -> Self {
        Self {
            processor,
            base_url: None,
        }
    }

    pub(crate) fn with_base_url(mut self, base_url: Option<Arc<Iri<String>>>) -> Self {
        self.base_url = base_url;
        self
    }

    /// The expanded document (§9.1 `expand` steps 7–8): a lone graph object without an
    /// identifier is unwrapped.
    pub(crate) fn expand_document(
        &mut self,
        active: &Arc<Context>,
        document: &Value<'_>,
    ) -> Result<Vec<Item>, JsonLdError> {
        if self.base_url.is_none() {
            self.base_url = active.original_base.clone();
        }
        Ok(match self.expand(active, None, document, false)? {
            Expanded::One(Item::Node(node)) if node.is_only_graph() => {
                node.graph.unwrap_or_default()
            }
            other => other.into_vec(),
        })
    }

    /// The Expansion algorithm.
    pub(crate) fn expand(
        &mut self,
        active: &Arc<Context>,
        property: Option<&str>,
        element: &Value<'_>,
        from_map: bool,
    ) -> Result<Expanded, JsonLdError> {
        match element {
            Value::Null => Ok(Expanded::Null),
            Value::Array(items) => Ok(Expanded::Many(
                self.expand_items(active, property, items, from_map)?,
            )),
            Value::Object(object) => self.expand_object(active, property, object, from_map),
            scalar => {
                let Some(property) = property.filter(|p| *p != "@graph") else {
                    // A free-floating scalar.
                    return Ok(Expanded::Null);
                };
                let mut active = active.clone();
                if let Some(definition) = active
                    .term(property)
                    .filter(|d| d.context.is_some())
                    .cloned()
                {
                    active = self.processor.scoped(&active, &definition, false, true)?;
                }
                Ok(self
                    .expand_value(&active, property, scalar)
                    .map_or(Expanded::Null, Expanded::One))
            }
        }
    }

    /// An array's items (§5.1.2 step 5).
    fn expand_items(
        &mut self,
        active: &Arc<Context>,
        property: Option<&str>,
        items: &[Value<'_>],
        from_map: bool,
    ) -> Result<Vec<Item>, JsonLdError> {
        let list = property
            .and_then(|p| active.term(p))
            .is_some_and(|d| d.container.has(Container::LIST));
        let mut result = Vec::with_capacity(items.len());
        for item in items {
            match self.expand(active, property, item, from_map)? {
                Expanded::Null => {}
                Expanded::One(item) => result.push(item),
                Expanded::Many(items) if list => {
                    if self.processor.is_1_0() {
                        return Err(error(
                            Code::ListOfLists,
                            "a list in a list, in JSON-LD 1.0 mode",
                        ));
                    }
                    result.push(Item::List(Box::new(ListObject { items, index: None })));
                }
                Expanded::Many(items) => result.extend(items),
            }
        }
        Ok(result)
    }

    /// Value Expansion (§5.3.2); `None` for an IRI that expands to nothing.
    fn expand_value(&self, active: &Context, property: &str, value: &Value<'_>) -> Option<Item> {
        let definition = active.term(property);
        let type_mapping = definition.and_then(|d| d.type_mapping.as_deref());
        if let Value::String(s) = value {
            match type_mapping {
                Some("@id") => {
                    let id = active.expand_iri(s, true, false).map(Iri_::into_shared);
                    return Some(Item::Node(Box::new(Node::reference(id))));
                }
                Some("@vocab") => {
                    let id = active.expand_iri(s, true, true).map(Iri_::into_shared);
                    return Some(Item::Node(Box::new(Node::reference(id))));
                }
                _ => {}
            }
        }
        let mut result = ValueObject {
            value: value.clone().into_owned(),
            datatype: None,
            language: None,
            direction: None,
            index: None,
        };
        match type_mapping {
            Some(t) if !matches!(t, "@id" | "@vocab" | "@none") => {
                result.datatype = definition.and_then(|d| d.type_mapping.clone());
            }
            _ if matches!(value, Value::String(_)) => {
                result.language = match definition.and_then(|d| d.language.as_ref()) {
                    Some(language) => language.clone(),
                    None => active.language.clone(),
                };
                result.direction = match definition.and_then(|d| d.direction) {
                    Some(direction) => direction,
                    None => active.direction,
                };
            }
            _ => {}
        }
        Some(Item::Value(Box::new(result)))
    }

    /// §5.1.2 steps 6 on: a map.
    fn expand_object(
        &mut self,
        active: &Arc<Context>,
        property: Option<&str>,
        element: &Object<'_>,
        from_map: bool,
    ) -> Result<Expanded, JsonLdError> {
        let mut active = active.clone();
        let property_definition = property.and_then(|p| active.term(p)).cloned();
        // Step 7: a non-propagated context ends at a new node object.
        if let Some(previous) = &active.previous
            && !from_map
        {
            let keeps = element.keys().any(|key| match active.expand_key(key) {
                Some(e) => e.as_str() == "@value" || (e.as_str() == "@id" && element.len() == 1),
                None => false,
            });
            if !keeps {
                active = previous.clone();
            }
        }
        // Step 8: the property-scoped context.
        if let Some(definition) = &property_definition
            && definition.context.is_some()
        {
            active = self.processor.scoped(&active, definition, true, true)?;
        }
        // Step 9: the element's own context.
        if let Some(local) = element.get("@context") {
            let base_url = self.base_url.clone();
            active = Arc::new(self.processor.process(
                &active,
                local,
                base_url.as_ref(),
                &mut Vec::new(),
                false,
                true,
                true,
            )?);
        }
        // Steps 10–11: type-scoped contexts, in code point order.
        let type_scoped = active.clone();
        let mut type_entries: Vec<(&str, &Value<'_>)> = element
            .iter()
            .filter(|(key, _)| {
                active
                    .expand_key(key)
                    .is_some_and(|e| e.as_str() == "@type")
            })
            .collect();
        type_entries.sort_unstable_by_key(|(key, _)| *key);
        for (_, value) in &type_entries {
            let mut terms: Vec<&str> = value.as_items().iter().filter_map(Value::as_str).collect();
            terms.sort_unstable();
            for term in terms {
                if let Some(definition) = type_scoped
                    .term(term)
                    .filter(|d| d.context.is_some())
                    .cloned()
                {
                    active = self.processor.scoped(&active, &definition, false, false)?;
                }
            }
        }
        // Step 12: whether the value of `@value` is JSON.
        let input_json = type_entries
            .first()
            .and_then(|(_, value)| value.as_items().last())
            .and_then(Value::as_str)
            .and_then(|t| active.expand_iri(t, false, true))
            .is_some_and(|e| e.as_str() == "@json");
        let mut result = Builder::default();
        let mut nests = Vec::new();
        self.expand_entries(
            &active,
            &type_scoped,
            property,
            element,
            input_json,
            &mut result,
            &mut nests,
        )?;
        self.expand_nests(
            &active,
            &type_scoped,
            element,
            &nests,
            input_json,
            &mut result,
        )?;
        self.finish(result, property)
    }

    /// Step 13: the entries of `element` into `result`; keys that expand to `@nest` are
    /// collected in `nests`.
    #[expect(clippy::too_many_arguments)]
    fn expand_entries<'e>(
        &mut self,
        active: &Arc<Context>,
        type_scoped: &Arc<Context>,
        property: Option<&str>,
        element: &'e Object<'_>,
        input_json: bool,
        result: &mut Builder,
        nests: &mut Vec<&'e str>,
    ) -> Result<(), JsonLdError> {
        for (key, value) in element.iter() {
            if key == "@context" {
                continue;
            }
            let Some(expanded) = active.expand_key(key) else {
                continue;
            };
            let expanded_property = expanded.as_str();
            if is_keyword(expanded_property) {
                self.keyword_entry(
                    active,
                    type_scoped,
                    property,
                    key,
                    expanded_property,
                    value,
                    input_json,
                    result,
                    nests,
                )?;
                continue;
            }
            if !expanded_property.contains(':') {
                continue;
            }
            let definition = active.term(key).cloned();
            let container = definition.as_ref().map(|d| d.container).unwrap_or_default();
            let mut expanded_value = if definition
                .as_ref()
                .is_some_and(|d| d.type_mapping.as_deref() == Some("@json"))
            {
                Expanded::One(Item::Value(Box::new(ValueObject {
                    value: value.clone().into_owned(),
                    datatype: Some(Str::from("@json")),
                    language: None,
                    direction: None,
                    index: None,
                })))
            } else if let (true, Value::Object(map)) = (container.has(Container::LANGUAGE), value) {
                Expanded::Many(self.language_map(active, key, map)?)
            } else if let (true, Value::Object(map)) = (
                container.has(Container::INDEX)
                    || container.has(Container::TYPE)
                    || container.has(Container::ID),
                value,
            ) {
                Expanded::Many(self.index_map(active, key, container, map)?)
            } else {
                self.expand(active, Some(key), value, false)?
            };
            if matches!(expanded_value, Expanded::Null) {
                continue;
            }
            if container.has(Container::LIST)
                && !matches!(expanded_value, Expanded::One(Item::List(_)))
            {
                expanded_value = Expanded::One(Item::List(Box::new(ListObject {
                    items: expanded_value.into_vec(),
                    index: None,
                })));
            }
            if container.has(Container::GRAPH)
                && !container.has(Container::ID)
                && !container.has(Container::INDEX)
            {
                expanded_value = Expanded::Many(
                    expanded_value
                        .into_vec()
                        .into_iter()
                        .map(Item::graph_of)
                        .collect(),
                );
            }
            let iri = expanded.into_shared();
            if definition.as_ref().is_some_and(|d| d.reverse) {
                result.keys |= key::REVERSE;
                for item in expanded_value.into_vec() {
                    if !matches!(item, Item::Node(_)) {
                        return Err(error(
                            Code::InvalidReversePropertyValue,
                            format!("the reverse property {key:?} has a value that isn't a node"),
                        ));
                    }
                    add(&mut result.reverse, iri.clone(), [item]);
                }
            } else {
                add(&mut result.properties, iri, expanded_value.into_vec());
            }
        }
        Ok(())
    }

    /// Step 13.4: an entry whose key expands to a keyword.
    #[expect(clippy::too_many_arguments)]
    fn keyword_entry<'e>(
        &mut self,
        active: &Arc<Context>,
        type_scoped: &Arc<Context>,
        property: Option<&str>,
        key: &'e str,
        keyword: &str,
        value: &Value<'_>,
        input_json: bool,
        result: &mut Builder,
        nests: &mut Vec<&'e str>,
    ) -> Result<(), JsonLdError> {
        if property == Some("@reverse") {
            return Err(error(
                Code::InvalidReversePropertyMap,
                format!("a reverse property map can't have the keyword {keyword}"),
            ));
        }
        let bit = key::of(keyword);
        let repeatable = keyword == "@included" || (keyword == "@type" && !self.processor.is_1_0());
        if result.keys & bit != 0 && !repeatable {
            return Err(error(
                Code::CollidingKeywords,
                format!("{keyword} is given twice"),
            ));
        }
        match keyword {
            "@id" => {
                let Value::String(id) = value else {
                    return Err(error(
                        Code::InvalidIdValue,
                        format!("@id must be a string, not {value}"),
                    ));
                };
                result.id = active.expand_iri(id, true, false).map(Iri_::into_shared);
                result.id_null = result.id.is_none();
                result.keys |= bit;
            }
            "@type" => {
                let types: Vec<&str> = match value {
                    Value::String(s) => vec![s],
                    Value::Array(items) => items
                        .iter()
                        .map(|item| item.as_str().ok_or(()))
                        .collect::<Result<_, _>>()
                        .map_err(|()| {
                            error(
                                Code::InvalidTypeValue,
                                format!("@type must be strings, not {value}"),
                            )
                        })?,
                    _ => {
                        return Err(error(
                            Code::InvalidTypeValue,
                            format!("@type must be strings, not {value}"),
                        ));
                    }
                };
                result.types_array |= matches!(value, Value::Array(_)) || result.keys & bit != 0;
                result.types.extend(
                    types
                        .into_iter()
                        .filter_map(|t| type_scoped.expand_iri(t, true, true))
                        .map(Iri_::into_shared),
                );
                result.keys |= bit;
            }
            "@graph" => {
                result.graph = Some(
                    self.expand(active, Some("@graph"), value, false)?
                        .into_vec(),
                );
                result.keys |= bit;
            }
            "@included" => {
                if self.processor.is_1_0() {
                    return Ok(());
                }
                // With `@included` as the active property, values that aren't node objects
                // stay to be rejected rather than vanish as free-floating.
                let items = self
                    .expand(active, Some("@included"), value, false)?
                    .into_vec();
                if items.iter().any(|item| !matches!(item, Item::Node(_))) {
                    return Err(error(
                        Code::InvalidIncludedValue,
                        "@included can only hold node objects",
                    ));
                }
                result.included.get_or_insert_with(Vec::new).extend(items);
                result.keys |= bit;
            }
            "@value" => {
                if input_json {
                    if self.processor.is_1_0() {
                        return Err(error(
                            Code::InvalidValueObjectValue,
                            "@json in JSON-LD 1.0 mode",
                        ));
                    }
                } else if !(value.is_scalar() || value.is_null()) {
                    return Err(error(
                        Code::InvalidValueObjectValue,
                        format!(
                            "@value must be a string, a number, a boolean or null, not {value}"
                        ),
                    ));
                }
                result.value = Some(value.clone().into_owned());
                result.keys |= bit;
            }
            "@language" => {
                let Value::String(language) = value else {
                    return Err(error(
                        Code::InvalidLanguageTaggedString,
                        format!("@language must be a string, not {value}"),
                    ));
                };
                result.language = Some(Str::from(language.as_ref()));
                result.keys |= bit;
            }
            "@direction" => {
                if self.processor.is_1_0() {
                    return Ok(());
                }
                let direction = value.as_str().and_then(Direction::parse).ok_or_else(|| {
                    error(
                        Code::InvalidBaseDirection,
                        format!("@direction must be ltr or rtl, not {value}"),
                    )
                })?;
                result.direction = Some(direction);
                result.keys |= bit;
            }
            "@index" => {
                let Value::String(index) = value else {
                    return Err(error(
                        Code::InvalidIndexValue,
                        format!("@index must be a string, not {value}"),
                    ));
                };
                result.index = Some(Str::from(index.as_ref()));
                result.keys |= bit;
            }
            "@list" => {
                if property.is_none_or(|p| p == "@graph") {
                    return Ok(());
                }
                let items = self.expand(active, property, value, false)?.into_vec();
                if self.processor.is_1_0() && items.iter().any(|i| matches!(i, Item::List(_))) {
                    return Err(error(
                        Code::ListOfLists,
                        "a list in a list, in JSON-LD 1.0 mode",
                    ));
                }
                result.list = Some(items);
                result.keys |= bit;
            }
            "@set" => {
                result.set = Some(self.expand(active, property, value, false)?);
                result.keys |= bit;
            }
            "@reverse" => {
                if !matches!(value, Value::Object(_)) {
                    return Err(error(
                        Code::InvalidReverseValue,
                        format!("@reverse must be an object, not {value}"),
                    ));
                }
                result.keys |= bit;
                let Expanded::One(Item::Node(node)) =
                    self.expand(active, Some("@reverse"), value, false)?
                else {
                    return Ok(());
                };
                let node = *node;
                // Reversed twice: forward.
                for (p, items) in node.reverse {
                    add(&mut result.properties, p, items);
                }
                for (p, items) in node.properties {
                    for item in items {
                        if !matches!(item, Item::Node(_)) {
                            return Err(error(
                                Code::InvalidReversePropertyValue,
                                format!("the reverse property {p} has a value that isn't a node"),
                            ));
                        }
                        add(&mut result.reverse, p.clone(), [item]);
                    }
                }
            }
            "@nest" => nests.push(key),
            // Other keywords (`@base`, `@vocab`, framing keywords, …) mean nothing here.
            _ => {}
        }
        Ok(())
    }

    /// Step 13.7: a language map.
    fn language_map(
        &mut self,
        active: &Arc<Context>,
        key: &str,
        map: &Object<'_>,
    ) -> Result<Vec<Item>, JsonLdError> {
        let definition = active.term(key);
        let direction = match definition.and_then(|d| d.direction) {
            Some(direction) => direction,
            None => active.direction,
        };
        let mut items = Vec::new();
        for (language, values) in map.iter() {
            let none = language == "@none"
                || active
                    .expand_iri(language, false, true)
                    .is_some_and(|e| e.as_str() == "@none");
            for value in values.as_items() {
                match value {
                    Value::Null => {}
                    Value::String(_) => items.push(Item::Value(Box::new(ValueObject {
                        value: value.clone().into_owned(),
                        datatype: None,
                        language: (!none).then(|| Str::from(language)),
                        direction,
                        index: None,
                    }))),
                    _ => {
                        return Err(error(
                            Code::InvalidLanguageMapValue,
                            format!("a language map value must be a string, not {value}"),
                        ));
                    }
                }
            }
        }
        Ok(items)
    }

    /// Step 13.8: an index, `@id` or `@type` map.
    fn index_map(
        &mut self,
        active: &Arc<Context>,
        key: &str,
        container: Container,
        map: &Object<'_>,
    ) -> Result<Vec<Item>, JsonLdError> {
        let index_key: Str = active
            .term(key)
            .and_then(|d| d.index.clone())
            .unwrap_or_else(|| Str::from("@index"));
        let mut result = Vec::new();
        for (index, values) in map.iter() {
            let mut map_context = active.clone();
            if (container.has(Container::ID) || container.has(Container::TYPE))
                && let Some(previous) = &active.previous
            {
                map_context = previous.clone();
            }
            if container.has(Container::TYPE)
                && let Some(definition) = map_context
                    .term(index)
                    .filter(|d| d.context.is_some())
                    .cloned()
            {
                map_context = self
                    .processor
                    .scoped(&map_context, &definition, false, true)?;
            }
            let expanded_index = active.expand_iri(index, false, true);
            let none = expanded_index
                .as_ref()
                .is_some_and(|e| e.as_str() == "@none");
            let items = self.expand_items(&map_context, Some(key), values.as_items(), true)?;
            for mut item in items {
                if container.has(Container::GRAPH)
                    && !matches!(&item, Item::Node(n) if n.is_graph_object())
                {
                    item = Item::graph_of(item);
                }
                if container.has(Container::INDEX) && &*index_key != "@index" && !none {
                    let index_value = Value::String(index.to_owned().into());
                    let reexpanded = self.expand_value(active, &index_key, &index_value);
                    let property = active
                        .expand_iri(&index_key, false, true)
                        .map(Iri_::into_shared);
                    match &mut item {
                        Item::Value(_) => {
                            return Err(error(
                                Code::InvalidValueObject,
                                format!(
                                    "a value in the property index {key:?} can't take the index as a property"
                                ),
                            ));
                        }
                        Item::Node(node) => {
                            if let (Some(reexpanded), Some(property)) = (reexpanded, property) {
                                match node.properties.iter_mut().find(|(p, _)| *p == property) {
                                    Some((_, values)) => values.insert(0, reexpanded),
                                    None => node.properties.push((property, vec![reexpanded])),
                                }
                            }
                        }
                        Item::List(_) => {}
                    }
                } else if container.has(Container::INDEX) && item.index().is_none() && !none {
                    item.set_index(Str::from(index));
                } else if container.has(Container::ID) && !none {
                    if let Item::Node(node) = &mut item
                        && node.id.is_none()
                    {
                        node.id = active.expand_iri(index, true, false).map(Iri_::into_shared);
                    }
                } else if container.has(Container::TYPE)
                    && !none
                    && let (Item::Node(node), Some(t)) = (&mut item, &expanded_index)
                {
                    node.types.insert(0, t.clone().into_shared());
                }
                result.push(item);
            }
        }
        Ok(result)
    }

    /// Step 14: the values of `@nest` keys, into the same result.
    fn expand_nests(
        &mut self,
        active: &Arc<Context>,
        type_scoped: &Arc<Context>,
        element: &Object<'_>,
        nests: &[&str],
        input_json: bool,
        result: &mut Builder,
    ) -> Result<(), JsonLdError> {
        for &nesting_key in nests {
            let Some(values) = element.get(nesting_key) else {
                continue;
            };
            for nested in values.as_items() {
                let Value::Object(nested) = nested else {
                    return Err(error(
                        Code::InvalidNestValue,
                        format!("a nested value must be an object, not {nested}"),
                    ));
                };
                if nested
                    .keys()
                    .any(|k| active.expand_key(k).is_some_and(|e| e.as_str() == "@value"))
                {
                    return Err(error(
                        Code::InvalidNestValue,
                        "a nested value can't be a value object",
                    ));
                }
                let mut context = active.clone();
                if let Some(definition) = active
                    .term(nesting_key)
                    .filter(|d| d.context.is_some())
                    .cloned()
                {
                    context = self.processor.scoped(&context, &definition, true, true)?;
                }
                let mut inner = Vec::new();
                self.expand_entries(
                    &context,
                    type_scoped,
                    Some(nesting_key),
                    nested,
                    input_json,
                    result,
                    &mut inner,
                )?;
                self.expand_nests(&context, type_scoped, nested, &inner, input_json, result)?;
            }
        }
        Ok(())
    }

    /// Steps 15–20: what the result map is.
    fn finish(&self, result: Builder, property: Option<&str>) -> Result<Expanded, JsonLdError> {
        let free_floating = property.is_none_or(|p| p == "@graph");
        if let Some(value) = result.value {
            let allowed = key::DIRECTION | key::INDEX | key::LANGUAGE | key::TYPE | key::VALUE;
            if result.keys & !allowed != 0
                || !result.properties.is_empty()
                || !result.reverse.is_empty()
            {
                return Err(error(
                    Code::InvalidValueObject,
                    "a value object has entries other than @value, @type, @language, @direction and @index",
                ));
            }
            if result.keys & key::TYPE != 0 && result.keys & (key::LANGUAGE | key::DIRECTION) != 0 {
                return Err(error(
                    Code::InvalidValueObject,
                    "a value object can't have both @type and @language or @direction",
                ));
            }
            let json =
                result.types.len() == 1 && &*result.types[0] == "@json" && !result.types_array;
            if !json {
                if value.is_null() {
                    return Ok(Expanded::Null);
                }
                if result.language.is_some() && !matches!(value, Value::String(_)) {
                    return Err(error(
                        Code::InvalidLanguageTaggedValue,
                        format!("only strings can have a language, not {value}"),
                    ));
                }
                if result.keys & key::TYPE != 0
                    && (result.types_array
                        || result.types.len() != 1
                        || !has_iri_form(&result.types[0])
                        || result.types[0].starts_with("_:")
                        || result.types[0].contains(char::is_whitespace))
                {
                    return Err(error(
                        Code::InvalidTypedValue,
                        "the @type of a value must be one IRI",
                    ));
                }
            }
            if free_floating {
                return Ok(Expanded::Null);
            }
            return Ok(Expanded::One(Item::Value(Box::new(ValueObject {
                value,
                datatype: result.types.into_iter().next(),
                language: result.language,
                direction: result.direction,
                index: result.index,
            }))));
        }
        if result.keys & (key::SET | key::LIST) != 0 {
            let others = result.keys & !(key::SET | key::LIST | key::INDEX);
            if others != 0
                || (result.keys & key::SET != 0 && result.keys & key::LIST != 0)
                || !result.properties.is_empty()
                || !result.reverse.is_empty()
            {
                return Err(error(
                    Code::InvalidSetOrListObject,
                    "a set or list object can only have @index besides",
                ));
            }
            if let Some(set) = result.set {
                return Ok(set);
            }
            if free_floating {
                return Ok(Expanded::Null);
            }
            return Ok(Expanded::One(Item::List(Box::new(ListObject {
                items: result.list.unwrap_or_default(),
                index: result.index,
            }))));
        }
        if result.keys == key::LANGUAGE && result.properties.is_empty() {
            return Ok(Expanded::Null);
        }
        let node = Node {
            id: result.id,
            id_null: result.id_null,
            types: result.types,
            properties: result.properties,
            reverse: result.reverse,
            graph: result.graph,
            included: result.included,
            index: result.index,
        };
        if free_floating {
            let empty = node.types.is_empty()
                && node.properties.is_empty()
                && node.reverse.is_empty()
                && node.graph.is_none()
                && node.included.is_none()
                && node.index.is_none();
            if empty {
                return Ok(Expanded::Null);
            }
        }
        Ok(Expanded::One(Item::Node(Box::new(node))))
    }
}
