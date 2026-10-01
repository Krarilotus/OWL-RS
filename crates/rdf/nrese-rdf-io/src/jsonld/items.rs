//! The expanded form as Rust types: node, value and list objects (`@set` objects don't
//! survive expansion). IRIs and keywords are shared `Arc<str>`s.

use std::borrow::Cow;

use nrese_json::{Object, Value};

use super::context::{Direction, Str};

#[derive(Debug, Clone)]
pub(crate) enum Item {
    Node(Box<Node>),
    Value(Box<ValueObject>),
    List(Box<ListObject>),
}

/// A node object, or a graph object (a node with `@graph` and at most `@id` and `@index`).
#[derive(Debug, Clone, Default)]
pub(crate) struct Node {
    pub(crate) id: Option<Str>,
    /// `@id` was given but expands to nothing (it has the form of a keyword): the node is
    /// kept, and has no triples.
    pub(crate) id_null: bool,
    pub(crate) types: Vec<Str>,
    /// In the order first seen; each property once.
    pub(crate) properties: Vec<(Str, Vec<Item>)>,
    /// Reverse properties: their values are node objects.
    pub(crate) reverse: Vec<(Str, Vec<Item>)>,
    pub(crate) graph: Option<Vec<Item>>,
    pub(crate) included: Option<Vec<Item>>,
    pub(crate) index: Option<Str>,
}

/// A value object: a JSON scalar (any JSON for `@json`), with its datatype or language.
#[derive(Debug, Clone)]
pub(crate) struct ValueObject {
    pub(crate) value: Value<'static>,
    /// A datatype IRI, or `@json`.
    pub(crate) datatype: Option<Str>,
    pub(crate) language: Option<Str>,
    pub(crate) direction: Option<Direction>,
    pub(crate) index: Option<Str>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ListObject {
    pub(crate) items: Vec<Item>,
    pub(crate) index: Option<Str>,
}

/// Appends `items` to `property`'s values in `properties`.
pub(crate) fn add(
    properties: &mut Vec<(Str, Vec<Item>)>,
    property: Str,
    items: impl IntoIterator<Item = Item>,
) {
    match properties.iter_mut().find(|(p, _)| *p == property) {
        Some((_, values)) => values.extend(items),
        None => properties.push((property, items.into_iter().collect())),
    }
}

impl Node {
    /// A node with only an identifier: what a value that is an IRI expands to.
    pub(crate) fn reference(id: Option<Str>) -> Self {
        Self {
            id_null: id.is_none(),
            id,
            ..Self::default()
        }
    }

    pub(crate) fn is_graph_object(&self) -> bool {
        self.graph.is_some()
            && self.types.is_empty()
            && self.properties.is_empty()
            && self.reverse.is_empty()
            && self.included.is_none()
    }

    /// A graph object with nothing but `@graph`.
    pub(crate) fn is_only_graph(&self) -> bool {
        self.is_graph_object() && self.id.is_none() && !self.id_null && self.index.is_none()
    }
}

impl Item {
    pub(crate) fn index(&self) -> Option<&Str> {
        match self {
            Self::Node(n) => n.index.as_ref(),
            Self::Value(v) => v.index.as_ref(),
            Self::List(l) => l.index.as_ref(),
        }
    }

    pub(crate) fn set_index(&mut self, index: Str) {
        match self {
            Self::Node(n) => n.index = Some(index),
            Self::Value(v) => v.index = Some(index),
            Self::List(l) => l.index = Some(index),
        }
    }

    pub(crate) fn graph_of(item: Item) -> Item {
        Item::Node(Box::new(Node {
            graph: Some(vec![item]),
            ..Node::default()
        }))
    }
}

/// The items as expanded JSON-LD.
pub(crate) fn to_json(items: &[Item]) -> Value<'static> {
    Value::Array(items.iter().map(item_json).collect())
}

fn text(s: &str) -> Value<'static> {
    Value::String(Cow::Owned(s.to_owned()))
}

fn properties_json(object: &mut Object<'static>, properties: &[(Str, Vec<Item>)]) {
    for (property, values) in properties {
        object.insert(Cow::Owned(property.to_string()), to_json(values));
    }
}

fn item_json(item: &Item) -> Value<'static> {
    let mut object = Object::new();
    match item {
        Item::Node(node) => {
            if let Some(id) = &node.id {
                object.insert("@id", text(id));
            } else if node.id_null {
                object.insert("@id", Value::Null);
            }
            if !node.types.is_empty() {
                object.insert(
                    "@type",
                    Value::Array(node.types.iter().map(|t| text(t)).collect()),
                );
            }
            if let Some(index) = &node.index {
                object.insert("@index", text(index));
            }
            if let Some(graph) = &node.graph {
                object.insert("@graph", to_json(graph));
            }
            if let Some(included) = &node.included {
                object.insert("@included", to_json(included));
            }
            if !node.reverse.is_empty() {
                let mut reverse = Object::new();
                properties_json(&mut reverse, &node.reverse);
                object.insert("@reverse", Value::Object(reverse));
            }
            properties_json(&mut object, &node.properties);
        }
        Item::Value(value) => {
            object.insert("@value", value.value.clone());
            if let Some(datatype) = &value.datatype {
                object.insert("@type", text(datatype));
            }
            if let Some(language) = &value.language {
                object.insert("@language", text(language));
            }
            if let Some(direction) = value.direction {
                object.insert("@direction", text(direction.as_str()));
            }
            if let Some(index) = &value.index {
                object.insert("@index", text(index));
            }
        }
        Item::List(list) => {
            object.insert("@list", to_json(&list.items));
            if let Some(index) = &list.index {
                object.insert("@index", text(index));
            }
        }
    }
    Value::Object(object)
}
