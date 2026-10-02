//! Serialize RDF as JSON-LD (JSON-LD 1.1 API §8.4) and RDF to Object (§8.5): a dataset
//! to an expanded JSON-LD document, collections as `@list`.
//!
//! It needs the whole dataset (a list is found from its end), so the serialiser collects
//! the quads until `finish`. The output is ordered (subjects, properties and graphs in
//! code point order), so the same dataset always gives the same text.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use nrese_json::canonical::write_number;
use nrese_json::{Object, Value};
use nrese_rdf::vocab::{rdf, xsd};
use nrese_rdf::{GraphNameRef, NamedOrBlankNodeRef, QuadRef, TermRef};

use super::{JsonLdError, JsonLdErrorCode as Code, JsonLdProcessingMode, RdfDirection};

const I18N: &str = "https://www.w3.org/ns/i18n#";
const RDF_LANGUAGE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#language";
const RDF_DIRECTION: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#direction";

/// The options of Serialize RDF as JSON-LD.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FromRdfOptions {
    /// `xsd:integer`, `xsd:double` and `xsd:boolean` literals as JSON numbers and booleans.
    pub use_native_types: bool,
    /// `rdf:type` as a property rather than `@type`.
    pub use_rdf_type: bool,
    /// Literals with a base direction written as `i18n-datatype` or `compound-literal`.
    pub rdf_direction: Option<RdfDirection>,
    pub processing_mode: JsonLdProcessingMode,
}

/// A node of the node map.
#[derive(Default)]
struct Node {
    id: String,
    types: Vec<String>,
    properties: BTreeMap<String, Vec<Value<'static>>>,
}

/// Where a value is: the subject of its node, its property, and its position.
type Usage = (String, String, usize);

fn node_id(node: NamedOrBlankNodeRef<'_>) -> String {
    match node {
        NamedOrBlankNodeRef::NamedNode(n) => n.as_str().to_owned(),
        NamedOrBlankNodeRef::BlankNode(b) => format!("_:{}", b.as_str()),
    }
}

fn text(s: &str) -> Value<'static> {
    Value::String(Cow::Owned(s.to_owned()))
}

fn reference(id: &str) -> Value<'static> {
    Value::Object(Object::from_iter([("@id", text(id))]))
}

/// The expanded JSON-LD document of `quads`.
pub fn from_rdf<'a>(
    quads: impl IntoIterator<Item = QuadRef<'a>>,
    options: &FromRdfOptions,
) -> Result<Value<'static>, JsonLdError> {
    let mut graphs: BTreeMap<String, BTreeMap<String, Node>> = BTreeMap::new();
    graphs.insert("@default".to_owned(), BTreeMap::new());
    let mut referenced_once: HashMap<String, Option<Usage>> = HashMap::new();
    let mut compound: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut nil_usages: BTreeMap<String, Vec<Usage>> = BTreeMap::new();
    let compound_mode = options.rdf_direction == Some(RdfDirection::CompoundLiteral);
    for quad in quads {
        let name = match quad.graph_name {
            GraphNameRef::DefaultGraph => "@default".to_owned(),
            GraphNameRef::NamedNode(n) => n.as_str().to_owned(),
            GraphNameRef::BlankNode(b) => format!("_:{}", b.as_str()),
        };
        if name != "@default" {
            let default = graphs.entry("@default".to_owned()).or_default();
            default.entry(name.clone()).or_insert_with(|| Node {
                id: name.clone(),
                ..Node::default()
            });
        }
        let graph = graphs.entry(name.clone()).or_default();
        let subject = node_id(quad.subject);
        graph.entry(subject.clone()).or_insert_with(|| Node {
            id: subject.clone(),
            ..Node::default()
        });
        if compound_mode && quad.predicate.as_str() == RDF_DIRECTION {
            compound
                .entry(name.clone())
                .or_default()
                .insert(subject.clone());
        }
        let object_node = NamedOrBlankNodeRef::try_from(quad.object).ok();
        if let Some(object) = object_node {
            let id = node_id(object);
            graph.entry(id.clone()).or_insert_with(|| Node {
                id,
                ..Node::default()
            });
        }
        let node = graph
            .get_mut(&subject)
            .unwrap_or_else(|| unreachable!("inserted above"));
        if quad.predicate == rdf::TYPE
            && !options.use_rdf_type
            && let Some(object) = object_node
        {
            let id = node_id(object);
            if !node.types.contains(&id) {
                node.types.push(id);
            }
            continue;
        }
        let value = rdf_to_object(quad.object, options)?;
        let values = node
            .properties
            .entry(quad.predicate.as_str().to_owned())
            .or_default();
        // A dataset is a set: a statement given twice is not a second use of its object.
        if values.contains(&value) {
            continue;
        }
        values.push(value);
        let position = values.len() - 1;
        let usage = (subject, quad.predicate.as_str().to_owned(), position);
        if quad.object == TermRef::NamedNode(rdf::NIL) {
            nil_usages.entry(name).or_default().push(usage);
        } else if let Some(NamedOrBlankNodeRef::BlankNode(b)) = object_node {
            let id = format!("_:{}", b.as_str());
            referenced_once
                .entry(id)
                .and_modify(|once| *once = None)
                .or_insert(Some(usage));
        } else if let Some(object) = object_node
            && let Some(once) = referenced_once.get_mut(&node_id(object))
        {
            *once = None;
        }
    }
    for (name, graph) in &mut graphs {
        if let Some(subjects) = compound.get(name) {
            for cl in subjects {
                compound_literal(graph, cl, referenced_once.get(cl).cloned().flatten())?;
            }
        }
        convert_lists(
            graph,
            &referenced_once,
            nil_usages.remove(name).unwrap_or_default(),
        );
    }
    let mut default = graphs.remove("@default").unwrap_or_default();
    let mut result = Vec::new();
    for (subject, node) in std::mem::take(&mut default) {
        let mut object = node_json(node);
        if let Some(graph) = graphs.remove(&subject) {
            let items: Vec<Value<'static>> = graph
                .into_values()
                .filter_map(|n| {
                    let o = node_json(n);
                    (o.len() > 1).then_some(Value::Object(o))
                })
                .collect();
            object.insert("@graph", Value::Array(items));
        }
        if object.len() > 1 {
            result.push(Value::Object(object));
        }
    }
    Ok(Value::Array(result))
}

fn node_json(node: Node) -> Object<'static> {
    let mut object = Object::new();
    object.insert("@id", text(&node.id));
    if !node.types.is_empty() {
        object.insert(
            "@type",
            Value::Array(node.types.iter().map(|t| text(t)).collect()),
        );
    }
    for (property, values) in node.properties {
        object.insert(Cow::Owned(property), Value::Array(values));
    }
    object
}

/// RDF to Object (§8.5).
fn rdf_to_object(
    term: TermRef<'_>,
    options: &FromRdfOptions,
) -> Result<Value<'static>, JsonLdError> {
    let literal = match term {
        TermRef::NamedNode(n) => return Ok(reference(n.as_str())),
        TermRef::BlankNode(b) => return Ok(reference(&format!("_:{}", b.as_str()))),
        TermRef::Literal(literal) => literal,
        TermRef::Triple(triple) => {
            return Err(JsonLdError::new(
                Code::UnsupportedTripleTerm,
                format!("JSON-LD 1.1 can't express the triple term <<( {triple} )>>"),
            ));
        }
    };
    let mut result = Object::new();
    let datatype = literal.datatype();
    let lexical = literal.value();
    let mut converted: Option<Value<'static>> = None;
    let mut type_: Option<String> = None;
    if options.use_native_types {
        if datatype == xsd::STRING {
            converted = Some(text(lexical));
        } else if datatype == xsd::BOOLEAN {
            converted = match lexical {
                "true" | "1" => Some(Value::Boolean(true)),
                "false" | "0" => Some(Value::Boolean(false)),
                _ => None,
            };
        } else if ((datatype == xsd::INTEGER && is_xsd_integer(lexical))
            || (datatype == xsd::DOUBLE && is_xsd_double(lexical)))
            && let Ok(x) = lexical.parse::<f64>()
            && x.is_finite()
        {
            let mut number = String::new();
            write_number(x, &mut number);
            converted = Some(Value::Number(Cow::Owned(number)));
        }
    }
    let value = match converted {
        Some(value) => value,
        None => {
            if options.processing_mode != JsonLdProcessingMode::JsonLd10 && datatype == rdf::JSON {
                type_ = Some("@json".to_owned());
                Value::parse(lexical)
                    .map_err(|e| {
                        JsonLdError::new(Code::InvalidJsonLiteral, format!("{lexical:?}: {e}"))
                    })?
                    .into_owned()
            } else if options.rdf_direction == Some(RdfDirection::I18nDatatype)
                && let Some(fragment) = datatype.as_str().strip_prefix(I18N)
            {
                let (language, direction) = fragment.split_once('_').unwrap_or((fragment, ""));
                if !language.is_empty() {
                    result.insert("@language", text(language));
                }
                result.insert("@direction", text(direction));
                text(lexical)
            } else if let Some(language) = literal.language() {
                result.insert("@language", text(language));
                if let Some(direction) = literal.direction() {
                    // An RDF 1.2 directional literal: JSON-LD's own base direction.
                    result.insert("@direction", text(direction.as_str()));
                }
                text(lexical)
            } else {
                if datatype != xsd::STRING {
                    type_ = Some(datatype.as_str().to_owned());
                }
                text(lexical)
            }
        }
    };
    result.insert("@value", value);
    if let Some(t) = type_ {
        result.insert("@type", text(&t));
    }
    Ok(Value::Object(result))
}

fn is_xsd_integer(s: &str) -> bool {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn is_xsd_double(s: &str) -> bool {
    if matches!(s, "INF" | "+INF" | "-INF" | "NaN") {
        return true;
    }
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mantissa_ok = (!int.is_empty() || !frac.is_empty())
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
        && !(mantissa.contains('.') && int.is_empty() && frac.is_empty());
    mantissa_ok && exponent.is_none_or(is_xsd_integer)
}

/// The compound literal `cl` (a node with `rdf:direction`) back to a value object where it
/// is used once.
fn compound_literal(
    graph: &mut BTreeMap<String, Node>,
    cl: &str,
    usage: Option<Usage>,
) -> Result<(), JsonLdError> {
    let Some((node, property, _)) = usage else {
        return Ok(());
    };
    let Some(cl_node) = graph.remove(cl) else {
        return Ok(());
    };
    let first = |property: &str| -> Option<Value<'static>> {
        let value = cl_node
            .properties
            .get(property)?
            .first()?
            .as_object()?
            .get("@value")?;
        Some(value.clone())
    };
    let mut replacement = Object::new();
    if let Some(value) = first(rdf::VALUE.as_str()) {
        replacement.insert("@value", value);
    }
    if let Some(language) = first(RDF_LANGUAGE) {
        if !language
            .as_str()
            .is_some_and(nrese_rdf::language::is_well_formed)
        {
            return Err(JsonLdError::new(
                Code::InvalidLanguageTaggedString,
                format!("{language} is not a language tag"),
            ));
        }
        replacement.insert("@language", language);
    }
    if let Some(direction) = first(RDF_DIRECTION) {
        if !matches!(direction.as_str(), Some("ltr" | "rtl")) {
            return Err(JsonLdError::new(
                Code::InvalidBaseDirection,
                format!("{direction} is not ltr or rtl"),
            ));
        }
        replacement.insert("@direction", direction);
    }
    if let Some(values) = graph
        .get_mut(&node)
        .and_then(|n| n.properties.get_mut(&property))
    {
        for value in values.iter_mut() {
            if value
                .as_object()
                .and_then(|o| o.get("@id"))
                .and_then(Value::as_str)
                == Some(cl)
            {
                *value = Value::Object(replacement.clone());
            }
        }
    }
    Ok(())
}

/// A well-formed list node: a blank node used once, with one `rdf:first` and one
/// `rdf:rest`, and nothing else but `rdf:type rdf:List`.
fn is_list_node(node: &Node, referenced_once: &HashMap<String, Option<Usage>>) -> bool {
    node.id.starts_with("_:")
        && matches!(referenced_once.get(&node.id), Some(Some(_)))
        && node.properties.len() == 2
        && node
            .properties
            .get(rdf::FIRST.as_str())
            .is_some_and(|v| v.len() == 1)
        && node
            .properties
            .get(rdf::REST.as_str())
            .is_some_and(|v| v.len() == 1)
        && (node.types.is_empty() || node.types == [rdf::LIST.as_str()])
}

/// A list found from its end: where its head value is, and its nodes from the end.
struct Conversion {
    head: Usage,
    nodes: Vec<String>,
}

/// The lists that end where `usages` use `rdf:nil`, as `@list` values (§8.4 step 6.4).
///
/// The specification shares values by reference, so converting a list inside another
/// list's node also changes the outer list's copy. Here the lists are found first, on the
/// unchanged graph, and converted innermost first: an outer list then copies inner lists
/// already converted.
fn convert_lists(
    graph: &mut BTreeMap<String, Node>,
    referenced_once: &HashMap<String, Option<Usage>>,
    usages: Vec<Usage>,
) {
    let conversions: Vec<Conversion> = usages
        .into_iter()
        .map(|usage| walk(graph, referenced_once, usage))
        .collect();
    // Which conversion's list holds each list node, and so how deep each list is nested.
    let owner: HashMap<&str, usize> = conversions
        .iter()
        .enumerate()
        .flat_map(|(i, c)| c.nodes.iter().map(move |n| (n.as_str(), i)))
        .collect();
    let depth = |mut i: usize| {
        let mut depth = 0;
        while let Some(&parent) = owner.get(conversions[i].head.0.as_str()) {
            if parent == i || depth > conversions.len() {
                break;
            }
            depth += 1;
            i = parent;
        }
        depth
    };
    let mut order: Vec<usize> = (0..conversions.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(depth(i)));
    for &i in &order {
        let conversion = &conversions[i];
        let list: Vec<Value<'static>> = conversion
            .nodes
            .iter()
            .rev()
            .filter_map(|id| {
                Some(
                    graph
                        .get(id)?
                        .properties
                        .get(rdf::FIRST.as_str())?
                        .first()?
                        .clone(),
                )
            })
            .collect();
        let (subject, property, position) = &conversion.head;
        if let Some(Value::Object(head)) = graph
            .get_mut(subject)
            .and_then(|n| n.properties.get_mut(property))
            .and_then(|values| values.get_mut(*position))
        {
            head.remove("@id");
            head.insert("@list", Value::Array(list));
        }
    }
    for conversion in conversions {
        for id in conversion.nodes {
            graph.remove(&id);
        }
    }
}

/// From the use of `rdf:nil` back to the list's head, over well-formed list nodes.
fn walk(
    graph: &BTreeMap<String, Node>,
    referenced_once: &HashMap<String, Option<Usage>>,
    usage: Usage,
) -> Conversion {
    let mut head = usage;
    let mut nodes = Vec::new();
    while head.1 == rdf::REST.as_str() {
        let Some(node) = graph.get(&head.0) else {
            break;
        };
        if !is_list_node(node, referenced_once) {
            break;
        }
        nodes.push(node.id.clone());
        let Some(Some(next)) = referenced_once.get(&node.id).cloned() else {
            break;
        };
        head = next;
        if !head.0.starts_with("_:") {
            break;
        }
    }
    Conversion { head, nodes }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xsd_number_forms() {
        for s in ["1", "+1", "-0", "007"] {
            assert!(is_xsd_integer(s), "{s}");
        }
        for s in ["", "+", "1.0", "1e3"] {
            assert!(!is_xsd_integer(s), "{s}");
        }
        for s in ["1", "1.", ".5", "1.5E-3", "-INF", "NaN", "1e+2"] {
            assert!(is_xsd_double(s), "{s}");
        }
        for s in ["", ".", "e1", "1e", "inf", "1.2.3"] {
            assert!(!is_xsd_double(s), "{s}");
        }
    }
}
