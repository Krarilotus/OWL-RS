//! Private ERA artifact glue. No validation, store load, or RDF identity algorithm.
//! All input bytes remain retained. Runtime producer capture is supplied, never inferred.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};

use nrese_json::{Object, Value};
use nrese_rdf::rdfc::Rdfc10;
use nrese_rdf::{Dataset, GraphName, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use nrese_rdf_io::{RdfFormat, RdfParser};
use sha2::{Digest, Sha256};

const FILE_CAP: usize = 262_144;
const QUAD_CAP: usize = 4096;
const BLANK_CAP: usize = 128;
const WORK_CAP: u64 = 10_000;
const ANCHOR_NS: &str = "urn:nrese:private-era-anchor:";
type J = Value<'static>;
type E<T> = Result<T, String>;

fn object(fields: impl IntoIterator<Item = (&'static str, J)>) -> J {
    Value::Object(fields.into_iter().collect())
}
fn text(s: impl Into<String>) -> J {
    Value::from(s.into())
}
fn get<'a>(v: &'a J, key: &str) -> E<&'a J> {
    v.as_object()
        .and_then(|o| o.get(key))
        .ok_or_else(|| format!("missing:{key}"))
}
fn string<'a>(v: &'a J, key: &str) -> E<&'a str> {
    get(v, key)?
        .as_str()
        .ok_or_else(|| format!("not-string:{key}"))
}
fn array<'a>(v: &'a J, key: &str) -> E<&'a [J]> {
    get(v, key)?
        .as_array()
        .ok_or_else(|| format!("not-array:{key}"))
}
fn require(ok: bool, why: &str) -> E<()> {
    if ok { Ok(()) } else { Err(why.into()) }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn read(path: &Path, cap: usize) -> E<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("open:{e}"))?
        .take((cap + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read:{e}"))?;
    require(bytes.len() <= cap, "refused:file-byte-cap")?;
    Ok(bytes)
}
fn json(bytes: &[u8]) -> E<J> {
    Value::parse_bytes(bytes)
        .map(Value::into_owned)
        .map_err(|e| format!("json:{e}"))
}
fn bound_file(root: &Path, entry: &J) -> E<Vec<u8>> {
    let path = Path::new(string(entry, "path")?);
    require(
        !path.as_os_str().is_empty()
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "refused:artifact-relative-path",
    )?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let actual = root
        .join(path)
        .canonicalize()
        .map_err(|e| format!("artifact:{e}"))?;
    require(
        actual.starts_with(&root),
        "refused:artifact-outside-capture",
    )?;
    let bytes = read(&actual, FILE_CAP)?;
    require(
        digest(&bytes) == string(entry, "sha256")?,
        "artifact-sha256-mismatch",
    )?;
    Ok(bytes)
}
fn parse(bytes: &[u8], format: &str) -> E<Vec<Quad>> {
    let format = match format {
        "nquads" => RdfFormat::NQuads,
        "ntriples" => RdfFormat::NTriples,
        "turtle" => RdfFormat::Turtle,
        "trig" => RdfFormat::TriG,
        _ => return Err("unsupported-rdf-format".into()),
    };
    let mut out = Vec::new();
    for quad in RdfParser::from_format(format).for_slice(bytes) {
        out.push(quad.map_err(|e| format!("incomplete-rdf-parse:{e}"))?);
        require(out.len() <= QUAD_CAP, "refused:quad-cap")?;
    }
    Ok(out)
}
fn term(s: &str) -> E<Term> {
    let bytes = format!("<urn:era:term:s> <urn:era:term:p> {s} .");
    let quads = parse(bytes.as_bytes(), "ntriples")?;
    let [q] = quads.as_slice() else {
        return Err("not-one-rdf-term".into());
    };
    require(
        q.subject.to_string() == "<urn:era:term:s>" && q.predicate.as_str() == "urn:era:term:p",
        "not-one-rdf-term",
    )?;
    Ok(q.object.clone())
}
fn visit_term(t: &Term, blanks: &mut BTreeSet<String>) -> E<()> {
    match t {
        Term::BlankNode(b) => {
            blanks.insert(b.as_str().into());
        }
        Term::NamedNode(n) => require(
            !n.as_str().starts_with(ANCHOR_NS),
            "reserved-anchor-iri-collision",
        )?,
        Term::Triple(t) => {
            visit_node(&t.subject, blanks)?;
            require(
                !t.predicate.as_str().starts_with(ANCHOR_NS),
                "reserved-anchor-iri-collision",
            )?;
            visit_term(&t.object, blanks)?;
        }
        Term::Literal(l) => require(
            !l.datatype().as_str().starts_with(ANCHOR_NS),
            "reserved-anchor-iri-collision",
        )?,
    }
    Ok(())
}
fn visit_node(n: &NamedOrBlankNode, blanks: &mut BTreeSet<String>) -> E<()> {
    match n {
        NamedOrBlankNode::BlankNode(b) => {
            blanks.insert(b.as_str().into());
        }
        NamedOrBlankNode::NamedNode(n) => require(
            !n.as_str().starts_with(ANCHOR_NS),
            "reserved-anchor-iri-collision",
        )?,
    }
    Ok(())
}
fn blanks(quads: &[Quad]) -> E<BTreeSet<String>> {
    let mut nodes = BTreeSet::new();
    for q in quads {
        visit_node(&q.subject, &mut nodes)?;
        visit_term(&q.object, &mut nodes)?;
        require(
            !q.predicate.as_str().starts_with(ANCHOR_NS),
            "reserved-anchor-iri-collision",
        )?;
        match &q.graph_name {
            GraphName::BlankNode(b) => {
                nodes.insert(b.as_str().into());
            }
            GraphName::NamedNode(n) => require(
                !n.as_str().starts_with(ANCHOR_NS),
                "reserved-anchor-iri-collision",
            )?,
            GraphName::DefaultGraph => {}
        }
    }
    require(nodes.len() <= BLANK_CAP, "refused:blank-node-cap")?;
    Ok(nodes)
}
fn bind_node(n: &NamedOrBlankNode, map: &BTreeMap<String, String>) -> NamedOrBlankNode {
    match n {
        NamedOrBlankNode::BlankNode(b) if map.contains_key(b.as_str()) => {
            NamedNode::new_unchecked(&map[b.as_str()]).into()
        }
        other => other.clone(),
    }
}
fn bind_term(t: &Term, map: &BTreeMap<String, String>) -> Term {
    match t {
        Term::BlankNode(b) if map.contains_key(b.as_str()) => {
            NamedNode::new_unchecked(&map[b.as_str()]).into()
        }
        Term::Triple(t) => Triple::new(
            bind_node(&t.subject, map),
            t.predicate.clone(),
            bind_term(&t.object, map),
        )
        .into(),
        other => other.clone(),
    }
}
fn bind(quads: &[Quad], map: &BTreeMap<String, String>) -> Vec<Quad> {
    quads
        .iter()
        .map(|q| Quad {
            subject: bind_node(&q.subject, map),
            predicate: q.predicate.clone(),
            object: bind_term(&q.object, map),
            graph_name: match &q.graph_name {
                GraphName::BlankNode(b) if map.contains_key(b.as_str()) => {
                    NamedNode::new_unchecked(&map[b.as_str()]).into()
                }
                other => other.clone(),
            },
        })
        .collect()
}
fn canonical(quads: &[Quad]) -> E<(String, J)> {
    // Raw graphs were checked before substitution. Minted fixed anchor IRIs are
    // expected here; reapplying the raw-input collision guard would reject them.
    let dataset = Dataset::from_iter(quads.iter().cloned());
    let result = Rdfc10::new()
        .with_work_limit(WORK_CAP)
        .canonicalize(dataset.iter())
        .map_err(|e| format!("rdfc-refused:{e}"))?;
    let issued = result
        .issued
        .iter()
        .map(|(a, b)| {
            object([
                ("input", text(a.to_string())),
                ("issued", text(b.to_string())),
            ])
        })
        .collect();
    Ok((result.to_nquads(), Value::Array(issued)))
}
// JSON member order is immaterial; arrays remain ordered. This is not RDF normalization.
fn ordered(v: &J) -> J {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (k.to_owned(), ordered(v)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect::<Object<'static>>(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(ordered).collect()),
        _ => v.clone(),
    }
}
fn typed_term(v: &J, map: &BTreeMap<String, String>, literal: bool, iri: bool) -> E<J> {
    let t = term(v.as_str().ok_or("typed-term-not-string")?)?;
    let mut observed = BTreeSet::new();
    visit_term(&t, &mut observed)?;
    require(
        observed.iter().all(|n| map.contains_key(n)),
        "missing-typed-source-anchor",
    )?;
    require(
        !literal || matches!(t, Term::Literal(_)),
        "message-not-literal",
    )?;
    require(
        !iri || matches!(t, Term::NamedNode(_)),
        "path-or-component-not-iri",
    )?;
    Ok(text(bind_term(&t, map).to_string()))
}
fn path(v: &J, map: &BTreeMap<String, String>, depth: usize) -> E<J> {
    require(depth <= 128, "refused:typed-path-depth")?;
    if v.is_null() {
        return Ok(Value::Null);
    }
    let kind = string(v, "kind")?;
    let (field, body) = match kind {
        "Predicate" => ("iri", typed_term(get(v, "iri")?, map, false, true)?),
        "Sequence" | "Alternative" => {
            let parts = array(v, "items")?;
            require(
                !parts.is_empty() && parts.iter().all(|p| !p.is_null()),
                "empty-or-null-path-items",
            )?;
            (
                "items",
                Value::Array(
                    parts
                        .iter()
                        .map(|p| path(p, map, depth + 1))
                        .collect::<E<_>>()?,
                ),
            )
        }
        "Inverse" | "ZeroOrMore" | "OneOrMore" | "ZeroOrOne" => {
            require(!get(v, "inner")?.is_null(), "null-inner-path")?;
            ("inner", path(get(v, "inner")?, map, depth + 1)?)
        }
        _ => return Err("unknown-or-truncated-path".into()),
    };
    require(
        v.as_object().is_some_and(|o| o.len() == 2),
        "unretained-path-field",
    )?;
    Ok(object([("kind", text(kind)), (field, body)]))
}
fn typed_results(
    meta: &J,
    map: &BTreeMap<String, String>,
    omit: &BTreeSet<String>,
) -> E<Vec<String>> {
    let mut out = Vec::new();
    for row in array(meta, "results")? {
        require(
            row.as_object().is_some_and(|o| o.len() == 8),
            "not-eight-typed-fields",
        )?;
        let mut fields = Vec::new();
        for key in [
            "focus_node",
            "value",
            "source_shape",
            "component",
            "source_constraint",
            "severity",
        ] {
            let v = get(row, key)?;
            let optional = matches!(key, "value" | "source_constraint");
            fields.push((
                key,
                if optional && v.is_null() {
                    Value::Null
                } else {
                    typed_term(v, map, false, matches!(key, "component" | "severity"))?
                },
            ));
        }
        fields.push(("path", path(get(row, "path")?, map, 0)?));
        let messages = array(row, "messages")?
            .iter()
            .map(|v| typed_term(v, map, true, false))
            .collect::<E<Vec<_>>>()?
            .into_iter()
            .filter(|v| !omit.contains(v.as_str().unwrap()))
            .collect();
        // Raw observation retains message order; strict typed comparison also preserves it.
        fields.push(("messages", Value::Array(messages)));
        out.push(ordered(&object(fields)).to_string());
    }
    out.sort(); // bag, not set: duplicate result rows remain duplicate.
    Ok(out)
}
fn projection(
    meta: &J,
    source: &[Quad],
    report: &[Quad],
) -> E<(String, BTreeSet<String>, Vec<Quad>, J)> {
    let policy = get(meta, "projection")?;
    if policy.as_str() == Some("none") {
        return Ok((
            "none".into(),
            BTreeSet::new(),
            report.to_vec(),
            Value::Array(Vec::new()),
        ));
    }
    require(
        string(policy, "name")? == "source-message-preserving-generated-message-projection",
        "unsupported-named-projection",
    )?;
    require(
        policy.as_object().is_some_and(|o| o.len() == 2),
        "unretained-projection-field",
    )?;
    let originals: BTreeSet<Term> = source
        .iter()
        .filter(|q| q.predicate.as_str() == "http://www.w3.org/ns/shacl#message")
        .map(|q| q.object.clone())
        .collect();
    let mut omitted = BTreeSet::new();
    for value in array(policy, "generated_message_terms")? {
        let t = term(value.as_str().ok_or("projection-term-not-string")?)?;
        require(
            matches!(t, Term::Literal(_)),
            "projection-message-not-literal",
        )?;
        require(
            !originals.contains(&t),
            "refused:projection-removes-original-source-message",
        )?;
        require(
            report.iter().any(|q| {
                q.predicate.as_str() == "http://www.w3.org/ns/shacl#resultMessage" && q.object == t
            }),
            "projection-term-not-observed",
        )?;
        require(omitted.insert(t.to_string()), "duplicate-projection-term")?;
    }
    let mut kept = Vec::new();
    let mut removed = Vec::new();
    for q in report {
        if q.predicate.as_str() == "http://www.w3.org/ns/shacl#resultMessage"
            && omitted.contains(&q.object.to_string())
        {
            removed.push(text(format!("{q} .")));
        } else {
            kept.push(q.clone());
        }
    }
    Ok((
        "source-message-preserving-generated-message-projection".into(),
        omitted,
        kept,
        Value::Array(removed),
    ))
}
struct Prepared {
    source: String,
    source_inputs: Vec<String>,
    report: String,
    typed: Vec<String>,
    projection: String,
    metadata: J,
    receipts: J,
}
fn prepare(file: &Path, meta: &J) -> E<Prepared> {
    require(
        string(meta, "schema")? == "era-private-v1",
        "schema-version",
    )?;
    let kind = string(meta, "capture_kind")?;
    // No emitted runtime producer observation format is delivered with this
    // checker. A hash/nonempty file or caller call/kind strings is not capture.
    // N11's adopted callable is not an emitted source/load/call/export receipt.
    require(
        kind == "authored-parser-unit",
        "unsupported-runtime-producer-observation-format",
    )?;
    require(
        get(meta, "complete")?.as_bool() == Some(true),
        "incomplete-typed-capture",
    )?;
    require(
        string(get(meta, "operational_outcome")?, "kind")? == "validation-returned",
        "operational-non-report-outcome",
    )?;
    require(
        matches!(
            string(meta, "observation_role")?,
            "full-validation" | "full-post-state" | "introduced" | "persistent-after-rollback"
        ),
        "unknown-observation-role",
    )?;
    for key in ["conforms", "report_applicable", "applicable"] {
        require(
            get(meta, key)?.as_bool().is_some(),
            "non-boolean-report-metadata",
        )?;
    }
    require(
        array(meta, "failures")?
            .iter()
            .all(|v| v.as_str().is_some()),
        "non-string-failure",
    )?;
    for key in ["revision", "shapes"] {
        require(
            matches!(get(meta,key)?,Value::Number(n) if n.parse::<u64>().is_ok()),
            "non-integer-metadata",
        )?;
    }
    let root = file.parent().ok_or("observation-parent")?;
    let anchors_bytes = bound_file(root, get(meta, "anchors")?)?;
    let anchors = json(&anchors_bytes)?;
    require(
        string(&anchors, "schema")? == "era-private-anchors-v1",
        "unsupported-anchor-schema",
    )?;
    require(
        get(&anchors, "complete")?.as_bool() == Some(true),
        "incomplete-producer-anchors",
    )?;
    require(
        string(&anchors, "call_id")? == string(meta, "call_id")?
            && string(&anchors, "capture_kind")? == kind,
        "anchor-call-binding",
    )?;
    let mut observed = BTreeSet::new();
    let mut source_by_scope = Vec::new();
    let mut source_inputs = Vec::new();
    let mut total = 0;
    let mut retained = anchors_bytes.len();
    for entry in array(&anchors, "sources")? {
        let role = string(entry, "role")?;
        require(matches!(role, "data" | "shapes"), "unknown-input-role")?;
        source_inputs.push(format!("{}:{}", role, string(entry, "sha256")?));
        let bytes = bound_file(root, entry)?;
        retained += bytes.len();
        require(retained <= 2_097_152, "refused:retained-input-cap")?;
        let quads = parse(&bytes, string(entry, "format")?)?;
        total += quads.len();
        require(total <= QUAD_CAP, "refused:total-quad-cap")?;
        let scope = string(entry, "scope")?.to_owned();
        for b in blanks(&quads)? {
            observed.insert((scope.clone(), b));
        }
        source_by_scope.push((scope, quads));
    }
    require(!source_by_scope.is_empty(), "missing-source-graphs")?;
    let mut source_map: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut runtime_map = BTreeMap::new();
    let mut bound = BTreeSet::new();
    let mut anchor_ids = BTreeSet::new();
    for b in array(&anchors, "bindings")? {
        let scope = string(b, "scope")?.to_owned();
        let input = string(b, "input_blank")?.to_owned();
        let runtime = string(b, "runtime_blank")?.to_owned();
        require(
            kind != "authored-parser-unit" || runtime == input,
            "authored-unit-is-not-observed-AsWritten-mapping",
        )?;
        let id = format!("{ANCHOR_NS}{}", hex(string(b, "anchor")?.as_bytes()));
        require(
            bound.insert((scope.clone(), input.clone())),
            "duplicate-input-anchor",
        )?;
        require(
            anchor_ids.insert(id.clone()),
            "distinct-source-anchor-collapse",
        )?;
        require(
            runtime_map.insert(runtime, id.clone()).is_none(),
            "distinct-runtime-anchor-collapse",
        )?;
        source_map.entry(scope).or_default().insert(input, id);
    }
    require(bound == observed, "producer-input-map-incomplete")?;
    let mut source = Vec::new();
    for (scope, quads) in source_by_scope {
        source.extend(bind(
            &quads,
            source_map.get(&scope).unwrap_or(&BTreeMap::new()),
        ));
    }
    let report_entry = get(meta, "report")?;
    require(
        get(report_entry, "complete")?.as_bool() == Some(true),
        "incomplete-report-export",
    )?;
    let report_bytes = bound_file(root, report_entry)?;
    retained += report_bytes.len();
    require(retained <= 2_097_152, "refused:retained-input-cap")?;
    let report = parse(&report_bytes, string(report_entry, "format")?)?;
    require(total + report.len() <= QUAD_CAP, "refused:total-quad-cap")?;
    let fresh: BTreeSet<String> = array(&anchors, "fresh_report_nodes")?
        .iter()
        .map(|n| {
            n.as_str()
                .map(str::to_owned)
                .ok_or("fresh-node-not-string".into())
        })
        .collect::<E<_>>()?;
    require(
        fresh.iter().all(|n| !runtime_map.contains_key(n)),
        "source-fresh-node-collision",
    )?;
    let report_blanks = blanks(&report)?;
    require(
        report_blanks
            .iter()
            .all(|n| fresh.contains(n) || runtime_map.contains_key(n)),
        "missing-report-node-classification",
    )?;
    require(
        fresh.is_subset(&report_blanks),
        "unobserved-fresh-report-node",
    )?;
    let (projection, omit, projected_report, removed) = projection(meta, &source, &report)?;
    let typed = typed_results(meta, &runtime_map, &omit)?;
    let (source, source_issued) = canonical(&source)?;
    let (report, report_issued) = canonical(&bind(&projected_report, &runtime_map))?;
    let receipts = object([
        (
            "observed_parser_source_nodes",
            Value::Array(
                observed
                    .iter()
                    .map(|(scope, node)| {
                        object([("scope", text(scope)), ("input_blank", text(node))])
                    })
                    .collect(),
            ),
        ),
        (
            "observed_report_blank_nodes",
            Value::Array(report_blanks.iter().map(text).collect()),
        ),
        ("source_issued", source_issued),
        ("report_issued", report_issued),
        ("anchors", anchors),
        ("source_canonical_sha256", text(digest(source.as_bytes()))),
        ("projection_removed_rdf", removed),
        (
            "projection_removed_typed_message_terms",
            Value::Array(omit.iter().map(text).collect()),
        ),
        ("report_canonical_sha256", text(digest(report.as_bytes()))),
    ]);
    source_inputs.sort();
    Ok(Prepared {
        source,
        source_inputs,
        report,
        typed,
        projection,
        metadata: meta.clone(),
        receipts,
    })
}
/// Private existing-harness command. It compares artifacts; it never runs validation.
pub fn compare_paths(left: &Path, right: &Path) -> J {
    let load = |p: &Path| read(p, 131_072).and_then(|b| json(&b));
    let l = load(left);
    let r = load(right);
    let mut out = object([
        (
            "comparison_kind",
            text("anchored-report-and-typed-artifact-comparison"),
        ),
        ("projection", text("none")),
        ("verdict", text("UNQUALIFIED")),
        ("completeness", text("unqualified")),
        ("runtime_validation_executed", Value::Boolean(false)),
        ("full_graph_certificate", Value::Boolean(false)),
        (
            "limits",
            object([
                ("file_bytes", text(FILE_CAP.to_string())),
                ("total_quads", text(QUAD_CAP.to_string())),
                ("blank_nodes", text(BLANK_CAP.to_string())),
                ("rdfc_work", text(WORK_CAP.to_string())),
            ]),
        ),
    ]);
    let mut fields = out.as_object().unwrap().clone();
    for (name, meta) in [("left_observation", &l), ("right_observation", &r)] {
        if let Ok(meta) = meta {
            fields.insert(name, meta.clone());
        }
    }
    let result = (|| -> E<(Prepared, Prepared)> {
        Ok((
            prepare(left, l.as_ref().map_err(Clone::clone)?)?,
            prepare(right, r.as_ref().map_err(Clone::clone)?)?,
        ))
    })();
    match result {
        Err(why) => fields.insert("reason", text(why)),
        Ok((l, r)) => {
            let mut equal = l.source == r.source
                && l.source_inputs == r.source_inputs
                && l.report == r.report
                && l.typed == r.typed
                && l.projection == r.projection;
            for key in [
                "capture_kind",
                "observation_role",
                "conforms",
                "report_applicable",
                "applicable",
                "failures",
                "operational_outcome",
            ] {
                equal &= ordered(get(&l.metadata, key).unwrap())
                    == ordered(get(&r.metadata, key).unwrap());
            }
            fields.insert("verdict", text(if equal { "MATCH" } else { "DIFFERENT" }));
            fields.insert(
                "projection",
                object([
                    ("name", text(&l.projection)),
                    ("left", get(&l.metadata, "projection").unwrap().clone()),
                    ("right", get(&r.metadata, "projection").unwrap().clone()),
                ]),
            );
            fields.insert(
                "full_graph_certificate",
                Value::Boolean(l.projection == "none" && r.projection == "none"),
            );
            if l.projection != "none" || r.projection != "none" {
                fields.insert(
                    "comparison_kind",
                    text("required-field-structured-projection"),
                );
            }
            fields.insert(
                "completeness",
                text("complete-authored-unit-artifacts-not-runtime-qualification"),
            );
            fields.insert("metadata_policy",text("revision/shapes retained as producer metadata, not cross-engine numeric equality; role/failures/applicability/outcomes strict"));
            fields.insert("left_normalization", l.receipts);
            fields.insert("right_normalization", r.receipts);
        }
    }
    out = Value::Object(fields);
    out
}

#[cfg(test)]
#[path = "era_tests.rs"]
mod tests;
