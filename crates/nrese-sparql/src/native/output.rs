//! Direct result serialisation: SPARQL Query Results JSON, TSV and CSV written from id
//! tables.
//!
//! The general path decodes every id into an owned term, builds a solution per row and
//! hands it to the results serialiser. Here dictionary terms are written from their
//! borrowed views (no allocation), each distinct id is formatted once (a bounded cache of
//! the serialised bytes, since joins repeat ids), and output goes out in 64 KiB chunks.
//! Each term is written by `nrese_sparql_results::write_term`, so the bytes are those of
//! its serialiser, RDF 1.2 terms included.
//!
//! Large results are written by every core: blocks of rows are serialised in parallel,
//! each under one read lock of the dictionary, and written out in order a window of
//! blocks at a time, so output still streams and memory stays bounded.

use std::io::Write;

use nrese_engine::{Snapshot, TermId, TermView};
use nrese_exec::{IdMap, IdTable, UNDEF, computed_index};
use nrese_rdf::{BlankNodeRef, LiteralRef, NamedNodeRef, Term, TermRef, Variable};
use nrese_sparql_results::QueryResultsFormat;

/// Serialised terms kept per query.
const CACHE_ENTRIES: usize = 1 << 16;
/// Output is written in chunks of this size.
const CHUNK: usize = 1 << 16;
/// Rows serialised together, by one thread.
const BLOCK_ROWS: usize = 4096;
/// Blocks serialised in parallel before they are written, per thread.
const BLOCKS_PER_THREAD: usize = 4;

/// Writes `s` as a JSON string, escaped as `json-event-parser` does: `\` and `"`, and
/// control characters as `\b \f \n \r \t` or `\u00xx`; everything else verbatim.
fn escaped(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escape: &[u8] = match b {
            b'\\' => b"\\\\",
            b'"' => b"\\\"",
            0x08 => b"\\b",
            0x0C => b"\\f",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0..=0x1F => {
                out.extend_from_slice(&bytes[start..i]);
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.extend_from_slice(&[
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[(b >> 4) as usize],
                    HEX[(b & 15) as usize],
                ]);
                start = i + 1;
                continue;
            }
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..i]);
        out.extend_from_slice(escape);
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

/// The results formats written directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultsFormat {
    Json,
    Tsv,
    Csv,
}

/// A dictionary view as a borrowed term; `None` for a triple term, whose view is text.
fn view_term(view: TermView<'_>) -> Option<TermRef<'_>> {
    Some(match view {
        TermView::Iri(iri) => NamedNodeRef::new_unchecked(iri).into(),
        TermView::BlankNode(label) => BlankNodeRef::new_unchecked(label).into(),
        TermView::String(value) => LiteralRef::new_simple_literal(value).into(),
        TermView::LangString {
            value,
            language,
            direction: None,
        } => LiteralRef::new_language_tagged_literal_unchecked(value, language).into(),
        TermView::LangString {
            value,
            language,
            direction: Some(direction),
        } => LiteralRef::new_directional_language_tagged_literal_unchecked(
            value, language, direction,
        )
        .into(),
        TermView::Typed { value, datatype } => {
            LiteralRef::new_typed_literal(value, NamedNodeRef::new_unchecked(datatype)).into()
        }
        TermView::Triple => return None,
    })
}

impl ResultsFormat {
    fn results_format(self) -> QueryResultsFormat {
        match self {
            Self::Json => QueryResultsFormat::Json,
            Self::Tsv => QueryResultsFormat::Tsv,
            Self::Csv => QueryResultsFormat::Csv,
        }
    }
}

/// Serialises the solutions of a native SELECT.
pub(super) struct DirectResults<'a> {
    pub snapshot: &'a Snapshot,
    pub computed: &'a [Term],
    pub format: ResultsFormat,
    /// Announced in JSON's head: the results may use RDF 1.2.
    pub version: Option<&'static str>,
}

impl DirectResults<'_> {
    /// Appends id `id` (a stored, inline or computed term) in the format; `view` looks up
    /// dictionary terms ([`Snapshot::with_views`]).
    fn id<'v>(&self, id: u64, view: &'v dyn Fn(TermId) -> Option<TermView<'v>>, out: &mut Vec<u8>) {
        let format = self.format.results_format();
        if let Some(index) = computed_index(id) {
            if let Some(t) = self.computed.get(index as usize) {
                nrese_sparql_results::write_term(format, t.as_ref(), out);
            }
            return;
        }
        let id = TermId::from_raw(id);
        if let Some(term) = view(id).and_then(view_term) {
            nrese_sparql_results::write_term(format, term, out);
            return;
        }
        // Inline ids and triple terms: through the decoded term.
        if let Some(t) = self.snapshot.decode(id) {
            nrese_sparql_results::write_term(format, t.as_ref(), out);
        }
    }

    /// Appends the rows `rows` of `table` (`keys`: each column's JSON object key).
    fn rows(
        &self,
        table: &IdTable,
        rows: std::ops::Range<usize>,
        keys: &[Vec<u8>],
        buffer: &mut Vec<u8>,
    ) {
        let json = self.format == ResultsFormat::Json;
        let (separator, end_of_row): (u8, &[u8]) = match self.format {
            ResultsFormat::Json => (b',', b"}"),
            ResultsFormat::Tsv => (b'\t', b"\n"),
            ResultsFormat::Csv => (b',', b"\r\n"),
        };
        // Each distinct id's bytes, written once: where they lie in `buffer`, which only grows
        // while this cache lives.
        let mut cache: IdMap<(u32, u32)> = IdMap::default();
        self.snapshot.with_views(|view| {
            for row in rows {
                if json {
                    if row > 0 {
                        buffer.push(b',');
                    }
                    buffer.push(b'{');
                }
                let mut first = true;
                for (column, key) in keys.iter().enumerate() {
                    let id = table.get(row, column);
                    if json {
                        // Unbound variables are left out of the binding object.
                        if id == UNDEF {
                            continue;
                        }
                        if !first {
                            buffer.push(b',');
                        }
                    } else if column > 0 {
                        // Every column has a field; unbound ones are empty.
                        buffer.push(separator);
                    }
                    first = false;
                    if id == UNDEF {
                        continue;
                    }
                    buffer.extend_from_slice(key);
                    if let Some(&(start, end)) = cache.get(&id) {
                        buffer.extend_from_within(start as usize..end as usize);
                    } else {
                        let start = buffer.len();
                        self.id(id, view, buffer);
                        if cache.len() < CACHE_ENTRIES
                            && let (Ok(start), Ok(end)) =
                                (u32::try_from(start), u32::try_from(buffer.len()))
                        {
                            cache.insert(id, (start, end));
                        }
                    }
                }
                buffer.extend_from_slice(end_of_row);
            }
        });
    }

    /// Writes the result document; `alive` is checked before each write and each window
    /// of blocks (it fails the write when the query is cancelled).
    pub fn write(
        &self,
        vars: &[Variable],
        table: &IdTable,
        out: &mut dyn Write,
        alive: &dyn Fn() -> std::io::Result<()>,
        workers: &nrese_exec::workers::Workers,
    ) -> std::io::Result<()> {
        let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK + 4096);
        let json = self.format == ResultsFormat::Json;
        let (separator, end_of_row): (u8, &[u8]) = match self.format {
            ResultsFormat::Json => (b',', b"}"),
            ResultsFormat::Tsv => (b'\t', b"\n"),
            ResultsFormat::Csv => (b',', b"\r\n"),
        };
        match self.format {
            ResultsFormat::Json => {
                buffer.extend_from_slice(b"{\"head\":{\"vars\":[");
                for (i, v) in vars.iter().enumerate() {
                    if i > 0 {
                        buffer.push(b',');
                    }
                    escaped(v.as_str(), &mut buffer);
                }
                buffer.push(b']');
                if let Some(version) = self.version {
                    buffer.extend_from_slice(b",\"version\":");
                    escaped(version, &mut buffer);
                }
                buffer.extend_from_slice(b"},\"results\":{\"bindings\":[");
            }
            ResultsFormat::Tsv | ResultsFormat::Csv => {
                for (i, v) in vars.iter().enumerate() {
                    if i > 0 {
                        buffer.push(separator);
                    }
                    if self.format == ResultsFormat::Tsv {
                        buffer.push(b'?');
                    }
                    buffer.extend_from_slice(v.as_str().as_bytes());
                }
                buffer.extend_from_slice(end_of_row);
            }
        }
        // JSON object keys per variable, formatted once.
        let keys: Vec<Vec<u8>> = vars
            .iter()
            .map(|v| {
                let mut key = Vec::new();
                if json {
                    escaped(v.as_str(), &mut key);
                    key.push(b':');
                }
                key
            })
            .collect();
        let rows = table.len();
        let blocks = rows.div_ceil(BLOCK_ROWS);
        let block = |b: usize| b * BLOCK_ROWS..((b + 1) * BLOCK_ROWS).min(rows);
        if blocks <= 1 {
            self.rows(table, 0..rows, &keys, &mut buffer);
        } else {
            // A window of blocks serialised in parallel, then written in order. A
            // cancelled query stops within one chunk of output (and one window of work).
            let window = workers.width() * BLOCKS_PER_THREAD;
            for start in (0..blocks).step_by(window) {
                alive()?;
                let parts = workers.map_range(start..(start + window).min(blocks), |b| {
                    let mut part = Vec::with_capacity(CHUNK);
                    self.rows(table, block(b), &keys, &mut part);
                    part
                });
                for part in parts {
                    let mut rest = part.as_slice();
                    while !rest.is_empty() {
                        let (now, later) =
                            rest.split_at(CHUNK.saturating_sub(buffer.len()).min(rest.len()));
                        buffer.extend_from_slice(now);
                        rest = later;
                        if buffer.len() >= CHUNK {
                            alive()?;
                            out.write_all(&buffer)?;
                            buffer.clear();
                        }
                    }
                }
            }
        }
        if json {
            buffer.extend_from_slice(b"]}}");
        }
        alive()?;
        out.write_all(&buffer)
    }
}

/// The JSON document of a boolean result, as `nrese_sparql_results` writes it.
pub(super) fn boolean(value: bool, version: Option<&str>) -> Vec<u8> {
    let mut out = b"{\"head\":{".to_vec();
    if let Some(version) = version {
        out.extend_from_slice(b"\"version\":");
        escaped(version, &mut out);
    }
    out.extend_from_slice(if value {
        b"},\"boolean\":true}"
    } else {
        b"},\"boolean\":false}"
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CancellationToken;

    #[test]
    fn cancelled_completed_tables_write_no_bytes_even_when_empty_or_small() {
        let engine = nrese_engine::Engine::new(Default::default()).unwrap();
        let snapshot = engine.snapshot();
        for rows in [0, 1, BLOCK_ROWS] {
            let table = IdTable::from_rows(0, std::iter::repeat_n(&[][..], rows));
            for format in [ResultsFormat::Json, ResultsFormat::Tsv, ResultsFormat::Csv] {
                let token = CancellationToken::new();
                token.cancel();
                let alive = || {
                    if token.is_cancelled() {
                        Err(std::io::Error::other("cancelled"))
                    } else {
                        Ok(())
                    }
                };
                let mut bytes = Vec::new();
                assert!(
                    DirectResults {
                        snapshot: &snapshot,
                        computed: &[],
                        format,
                        version: None,
                    }
                    .write(&[], &table, &mut bytes, &alive, &Default::default())
                    .is_err()
                );
                assert!(bytes.is_empty());
            }
        }
    }

    #[test]
    fn an_answer_cancelled_during_its_final_write_is_written() {
        struct CancelOnWrite {
            token: CancellationToken,
            writes: usize,
            bytes: usize,
        }
        impl Write for CancelOnWrite {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.writes += 1;
                self.bytes += bytes.len();
                self.token.cancel();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let engine = nrese_engine::Engine::new(Default::default()).unwrap();
        let snapshot = engine.snapshot();
        let workers = nrese_exec::workers::Workers::current();
        // Two row blocks, but their zero-column JSON rows fit in the final byte buffer.
        let table = IdTable::from_rows(0, std::iter::repeat_n(&[][..], BLOCK_ROWS + 1));
        let token = CancellationToken::new();
        let mut writer = CancelOnWrite {
            token: token.clone(),
            writes: 0,
            bytes: 0,
        };
        let output = DirectResults {
            snapshot: &snapshot,
            computed: &[],
            format: ResultsFormat::Json,
            version: None,
        };
        let mut expected = Vec::new();
        output
            .write(&[], &table, &mut expected, &|| Ok(()), &workers)
            .unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&expected).unwrap();
        assert_eq!(
            decoded["results"]["bindings"].as_array().unwrap().len(),
            BLOCK_ROWS + 1
        );
        assert!(expected.len() < CHUNK);
        let result = output.write(
            &[],
            &table,
            &mut writer,
            &|| {
                if token.is_cancelled() {
                    Err(std::io::Error::other("cancelled"))
                } else {
                    Ok(())
                }
            },
            &workers,
        );
        assert!(result.is_ok());
        assert_eq!(writer.writes, 1, "cancellation occurs in the final write");
        assert_eq!(writer.bytes, expected.len());
    }

    /// Spec (module doc): each cell's bytes are those the results serialiser writes for its
    /// term, whatever the term cache does with it. Here, in each of two blocks: ids it keeps
    /// and hits, more distinct ids than it holds (so later ids, met once or recurring, are
    /// written each time), unbound cells, computed and inline ids, and every kind of term.
    #[test]
    fn blocks_past_the_term_cache_match_the_results_serialiser() {
        use nrese_rdf::vocab::xsd;
        use nrese_rdf::{BaseDirection, BlankNode, GraphName, Literal, NamedNode, Quad, Triple};
        use nrese_sparql_results::QueryResultsSerializer;

        let iri = |s: String| NamedNode::new_unchecked(format!("http://example.com/{s}"));
        // Columns: one of 16 recurring ids; a computed value; a recurring id or unbound; PAIRED
        // columns whose ids each recur once, on the next row, more of them than the cache
        // holds; a column of ids met once; one whose last 128 rows reuse 32 ids met after the
        // cache is full.
        const PAIRED: usize = 34;
        let width = 3 + PAIRED + 2;
        let paired = BLOCK_ROWS / 2 * PAIRED;
        let distinct = paired + BLOCK_ROWS;
        assert!(paired > CACHE_ENTRIES);
        let engine = nrese_engine::Engine::new(Default::default()).unwrap();
        let mut tx = engine.transaction();
        let terms: Vec<Term> = (0..distinct)
            .map(|n| match n % 8 {
                0 => iri(format!("i{n}\"q")).into(),
                1 => BlankNode::new_unchecked(format!("b{n}")).into(),
                2 => Literal::new_simple_literal(format!("v{n} \"\\\t\n,é")).into(),
                3 => {
                    Literal::new_language_tagged_literal_unchecked(format!("l{n}"), "en-gb").into()
                }
                4 => Literal::new_directional_language_tagged_literal_unchecked(
                    format!("d{n}"),
                    "ar",
                    if n % 16 == 4 {
                        BaseDirection::Rtl
                    } else {
                        BaseDirection::Ltr
                    },
                )
                .into(),
                5 => Literal::new_typed_literal(format!("c{n}"), iri("dt".into())).into(),
                6 => Triple::new(
                    iri(format!("s{n}")),
                    iri("p".into()),
                    Literal::new_typed_literal(n.to_string(), xsd::INTEGER),
                )
                .into(),
                _ => Literal::new_typed_literal(format!("+{n}"), xsd::INTEGER).into(),
            })
            .collect();
        for term in &terms {
            let quad = Quad::new(
                iri("s".into()),
                iri("o".into()),
                term.clone(),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let ids: Vec<u64> = terms
            .iter()
            .map(|t| snapshot.lookup(t.as_ref()).unwrap().raw())
            .collect();
        let computed: Vec<Term> = (0..3)
            .map(|n| Literal::new_simple_literal(format!("computed \"{n}\"\n")).into())
            .collect();
        let rows: Vec<Vec<u64>> = (0..2 * BLOCK_ROWS)
            .map(|r| {
                let i = r % BLOCK_ROWS;
                let mut row = vec![ids[r % 16], nrese_exec::computed_id((r % 3) as u64)];
                row.push(if r % 5 == 0 { UNDEF } else { ids[r % 7] });
                row.extend((0..PAIRED).map(|c| ids[i / 2 * PAIRED + c]));
                row.push(ids[paired + i]);
                row.push(if i < BLOCK_ROWS - 128 {
                    ids[0]
                } else {
                    ids[distinct - 1 - i % 32]
                });
                row
            })
            .collect();
        let vars: Vec<Variable> = (0..width)
            .map(|c| Variable::new_unchecked(format!("v{c}")))
            .collect();
        let workers = nrese_exec::workers::Workers::current();
        // One block (written into the document's buffer) and two (written in parallel).
        for blocks in [1, 2] {
            let rows = &rows[..blocks * BLOCK_ROWS];
            let table = IdTable::from_rows(width, rows.iter().map(Vec::as_slice));
            for format in [ResultsFormat::Json, ResultsFormat::Tsv, ResultsFormat::Csv] {
                let mut direct = Vec::new();
                DirectResults {
                    snapshot: &snapshot,
                    computed: &computed,
                    format,
                    version: None,
                }
                .write(&vars, &table, &mut direct, &|| Ok(()), &workers)
                .unwrap();
                let mut expected = Vec::new();
                let mut writer = QueryResultsSerializer::from_format(format.results_format())
                    .serialize_solutions_to_writer(&mut expected, vars.clone())
                    .unwrap();
                for row in rows {
                    let decoded: Vec<Option<Term>> = row
                        .iter()
                        .map(|&id| match computed_index(id) {
                            _ if id == UNDEF => None,
                            Some(index) => Some(computed[index as usize].clone()),
                            None => snapshot.decode(TermId::from_raw(id)),
                        })
                        .collect();
                    let row: Vec<Option<TermRef<'_>>> = decoded
                        .iter()
                        .map(|t| t.as_ref().map(Term::as_ref))
                        .collect();
                    writer.serialize_row(&row).unwrap();
                }
                writer.finish().unwrap();
                assert!(
                    direct == expected,
                    "{format:?}, {blocks} blocks: first difference at byte {:?}",
                    direct.iter().zip(&expected).position(|(a, b)| a != b)
                );
            }
        }
    }

    #[test]
    fn escaping_matches_json_event_parser() {
        let mut out = Vec::new();
        escaped("a\"b\\c\nd\te\u{1}f\u{1f}é/ü\u{7f}", &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\"a\\\"b\\\\c\\nd\\te\\u0001f\\u001fé/ü\u{7f}\""
        );
    }
}
