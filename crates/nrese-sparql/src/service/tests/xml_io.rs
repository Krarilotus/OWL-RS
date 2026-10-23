use super::*;
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParseError, QueryResultsParser, ReaderQueryResultsParserOutput,
};
use std::error::Error;
use std::io::{self, Read};

#[derive(Debug)]
struct OriginalReadError {
    operational: Option<QueryEvaluationError>,
    native: Option<Stop>,
    owner: Arc<()>,
}

impl std::fmt::Display for OriginalReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.operational {
            Some(error) => std::fmt::Display::fmt(error, f),
            None => match &self.native {
                Some(reason) => std::fmt::Display::fmt(reason, f),
                None => f.write_str("cancelled-looking ordinary upstream timeout"),
            },
        }
    }
}

impl Error for OriginalReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.operational
            .as_ref()
            .map(|error| error as &dyn Error)
            .or_else(|| self.native.as_ref().map(|reason| reason as &dyn Error))
    }
}

struct FailingXmlReader {
    prefix: io::Cursor<&'static [u8]>,
    error: Option<io::Error>,
}

impl Read for FailingXmlReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.prefix.read(bytes)?;
        if count != 0 {
            Ok(count)
        } else {
            Err(self
                .error
                .take()
                .expect("the XML reader stops at its I/O error"))
        }
    }
}

// Spec: Root's XML I/O contract23 October2026: the real parser preserves the
// original owner/kind/text and the SERVICE bridge keeps typed memory/cancellation.
// F1: native StopSource cancellation remains Cancelled without preconversion.
#[test]
fn n7_xml_reader_preserves_original_operational_source() {
    let prefix = br#"<sparql xmlns="http://www.w3.org/2005/sparql-results#"><head><variable name="x"/></head><results><result><binding name="x"><literal>"#;
    for mode in 0..6 {
        let expected_memory = match mode {
            0 | 4 => Some(Budget::new(2).charge(3).unwrap_err()),
            1 => Some(
                Budget::new(16)
                    .within(Some(nrese_exec::SharedBudget::new(2)))
                    .charge(3)
                    .unwrap_err(),
            ),
            _ => None,
        };
        let cancellation = if matches!(mode, 2 | 5) {
            let token = CancellationToken::new();
            token.cancel();
            Some(
                StopSource::new(Some(token.flag()), None)
                    .check()
                    .unwrap_err(),
            )
        } else {
            None
        };
        let operational = match mode {
            0 | 1 | 4 => Some(QueryEvaluationError::MemoryLimit(expected_memory.unwrap())),
            2 => Some(QueryEvaluationError::from(cancellation.unwrap())),
            _ => None,
        };
        let owner = Arc::new(());
        let weak = Arc::downgrade(&owner);
        // F1: keep the real native reason in the reader payload, without
        // converting it to QueryEvaluationError before the XML/bridge path.
        let native = if mode == 5 { cancellation } else { None };
        let original = OriginalReadError {
            operational,
            native,
            owner,
        };
        let expected_text = original.to_string();
        let kind = if mode == 3 {
            io::ErrorKind::TimedOut
        } else {
            io::ErrorKind::Other
        };
        let reader = FailingXmlReader {
            prefix: io::Cursor::new(prefix.as_slice()),
            error: Some(io::Error::new(kind, original)),
        };
        let mut shared_error = None;
        let error = if mode == 4 {
            // A real quick-xml read failure, retained twice when the results
            // conversion takes it. This also exercises shared Arc ownership.
            let mut reader = quick_xml::Reader::from_reader(io::BufReader::new(reader));
            let mut buffer = Vec::new();
            let error = loop {
                match reader.read_event_into(&mut buffer) {
                    Ok(quick_xml::events::Event::Eof) => panic!("failing reader cannot reach EOF"),
                    Ok(_) => buffer.clear(),
                    Err(error) => break error,
                }
            };
            shared_error = Some(error.clone());
            QueryResultsParseError::from(error)
        } else {
            let parsed = QueryResultsParser::from_format(QueryResultsFormat::Xml)
                .for_reader(reader)
                .expect("the valid XML head precedes the failing literal read");
            let ReaderQueryResultsParserOutput::Solutions(mut rows) = parsed else {
                panic!("XML SELECT head must produce solutions");
            };
            rows.next()
                .expect("a row read reaches the original I/O error")
                .unwrap_err()
        };
        drop(shared_error);
        let QueryResultsParseError::Io(ref io) = error else {
            panic!("reader failure is operational I/O, not XML syntax");
        };
        assert_eq!(io.kind(), kind);
        assert_eq!(io.to_string(), expected_text);

        // Public source/get_ref traversal, without assuming the adapter's depth
        // or private holder type. The marker is the reader's actual owned value.
        let mut cause: &(dyn Error + 'static) = &error;
        let original = loop {
            if let Some(original) = cause.downcast_ref::<OriginalReadError>() {
                break original;
            }
            cause = cause
                .downcast_ref::<io::Error>()
                .and_then(|error| error.get_ref().map(|error| error as &dyn Error))
                .or_else(|| cause.source())
                .expect("original XML read owner was lost");
        };
        assert!(std::ptr::eq(Arc::as_ptr(&original.owner), weak.as_ptr()));
        assert!(
            weak.upgrade().is_some(),
            "parse error retains its original source"
        );

        let mapped = super::super::json::parse_error(error);
        match (mode, &mapped) {
            (0 | 1 | 4, QueryEvaluationError::MemoryLimit(actual)) => {
                let expected = expected_memory.unwrap();
                assert_eq!(
                    (actual.limit, actual.used, actual.requested, actual.shared),
                    (
                        expected.limit,
                        expected.used,
                        expected.requested,
                        expected.shared
                    ),
                );
            }
            (2 | 5, QueryEvaluationError::Cancelled) => {}
            (5, actual) => panic!("native Stop::Cancelled was misclassified: {actual:?}"),
            (3, QueryEvaluationError::Service(_)) => {}
            _ => panic!("XML operational source was misclassified: {mapped}"),
        }
        assert_eq!(weak.upgrade().is_some(), mode == 3);
        drop(mapped);
        assert!(
            weak.upgrade().is_none(),
            "last error drop releases the original owner"
        );
    }
}
