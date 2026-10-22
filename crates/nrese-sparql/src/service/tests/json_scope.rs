//! Distinguishing scoped-JSON ownership, control and real collector work probes.
use super::*;
use nrese_json::{JsonEvent, JsonParseError, JsonSessionCallbacks, SliceJsonParser};
use nrese_sparql_results::{
    QueryResultsFormat, QueryResultsParseError, QueryResultsParser, SolutionsCallbacks,
};
use std::{
    cell::Cell,
    io,
    panic::{AssertUnwindSafe, catch_unwind},
};

#[derive(Debug)]
struct Marker;
impl std::fmt::Display for Marker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operational marker")
    }
}
impl std::error::Error for Marker {}

struct Lexical {
    lease: Reservation,
    checks: usize,
    fail_at: usize,
    reserves: usize,
    frees: usize,
}
impl Lexical {
    fn new(budget: &Arc<Budget>) -> Self {
        Self {
            lease: Reservation::new(Arc::clone(budget)),
            checks: 0,
            fail_at: usize::MAX,
            reserves: 0,
            frees: 0,
        }
    }
    fn hooks() -> JsonSessionCallbacks<Self> {
        JsonSessionCallbacks {
            reserve: |c, n| {
                c.lease.charge(n).map_err(io::Error::other)?;
                c.reserves += 1;
                Ok(())
            },
            free: |c, n| {
                c.lease.release(n);
                if n != 0 {
                    c.frees += 1;
                }
            },
            check: |c| {
                c.checks += 1;
                if c.checks == c.fail_at {
                    Err(io::Error::other(Marker))
                } else {
                    Ok(())
                }
            },
        }
    }
}

#[test]
fn n7_json_lexical_admission_overlap_and_scoped_unwind() {
    if !alone() {
        return;
    }
    let budget = Arc::new(Budget::new(1 << 20));
    let body = br#"{"plain":"borrowed","escaped":"a\nb\u20acc","nested":[[[[]]]]}"#;
    let ((reserves, frees), uncovered) = measure_within(&budget, || {
        let mut owner = Lexical::new(&budget);
        SliceJsonParser::with_session(body, &mut owner, &Lexical::hooks(), |cursor| {
            loop {
                let end = cursor.next_event(|_, event| Ok(matches!(event, JsonEvent::Eof)))?;
                if end {
                    break;
                }
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(owner.lease.bytes(), 0);
        (owner.reserves, owner.frees)
    });
    assert_eq!(
        uncovered, 0,
        "only lexer stack and escaped event are allocated here"
    );
    assert_eq!(reserves, 3, "stack4, escaped text, stack8 beside stack4");
    assert_eq!(reserves, frees);
    assert_eq!(budget.used(), 0);

    // Four old bool slots remain live while eight new slots are admitted.
    let budget = Arc::new(Budget::new(11));
    let mut owner = Lexical::new(&budget);
    let error =
        SliceJsonParser::with_session(b"[[[[[]]]]]", &mut owner, &Lexical::hooks(), |cursor| {
            loop {
                if cursor.next_event(|_, e| Ok(matches!(e, JsonEvent::Eof)))? {
                    break;
                }
            }
            Ok(())
        })
        .unwrap_err();
    assert!(
        matches!(error, JsonParseError::Io(ref e) if e.get_ref().is_some_and(|e| e.is::<nrese_exec::BudgetExceeded>()))
    );
    assert_eq!(budget.peak(), 4);
    assert_eq!(budget.used(), 0);

    let budget = Arc::new(Budget::new(1 << 20));
    let mut owner = Lexical::new(&budget);
    let panic = catch_unwind(AssertUnwindSafe(|| {
        let _: Result<(), _> =
            SliceJsonParser::with_session(br#""a\nb""#, &mut owner, &Lexical::hooks(), |cursor| {
                cursor.next_event(|_, event| {
                    assert!(matches!(event, JsonEvent::String(_)));
                    panic!("consumer unwind")
                })
            });
    }));
    assert!(panic.is_err());
    assert_eq!(owner.lease.bytes(), 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn n7_json_lexical_bounded_scan_preserves_operational_io() {
    let cases = [
        format!("\"{}\"", "x".repeat(30_000)),
        format!("\"{}\"", "\\n".repeat(20_000)),
        format!("{}0", " ".repeat(30_000)),
        format!("1{}", "0".repeat(30_000)),
    ];
    for body in cases {
        let budget = Arc::new(Budget::new(1 << 20));
        let mut owner = Lexical::new(&budget);
        owner.fail_at = 3; // Two explicit entry/UTF8 checks pass; fail inside next_event.
        let error = SliceJsonParser::with_session(
            body.as_bytes(),
            &mut owner,
            &Lexical::hooks(),
            |cursor| {
                cursor
                    .next_event::<()>(|_, _| panic!("scan must stop before an event is published"))
            },
        )
        .unwrap_err();
        assert!(
            matches!(error, JsonParseError::Io(ref e) if e.kind() == io::ErrorKind::Other && e.get_ref().is_some_and(|e| e.is::<Marker>()))
        );
        assert_eq!(
            owner.checks, 4,
            "two entry checks, internal failure, closing check"
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn n7_json_plain_borrows_and_escaped_temporary_is_not_a_move_claim() {
    if !alone() {
        return;
    }
    for (body, reserves) in [(br#""plain""#.as_slice(), 0), (br#""a\nb""#.as_slice(), 1)] {
        let budget = Arc::new(Budget::new(1 << 20));
        let ((calls, frees), uncovered) = measure_within(&budget, || {
            let mut owner = Lexical::new(&budget);
            SliceJsonParser::with_session(body, &mut owner, &Lexical::hooks(), |cursor| {
                cursor.next_event(|_, event| {
                    let JsonEvent::String(value) = event else {
                        panic!("string")
                    };
                    assert!(matches!(value, std::borrow::Cow::Borrowed(_)));
                    assert_eq!(value, if reserves == 0 { "plain" } else { "a\nb" });
                    Ok(())
                })
            })
            .unwrap();
            (owner.reserves, owner.frees)
        });
        assert_eq!(uncovered, 0);
        assert_eq!(calls, reserves);
        assert_eq!(frees, reserves);
        assert_eq!(budget.used(), 0);
        assert_eq!(budget.peak(), if reserves == 0 { 0 } else { 4 });
    }
}

#[test]
fn n7_json_ordinary_resume_reader_and_owned_event_semantics() {
    let text = r#"["plain","a\nb"]"#;
    let mut slice = SliceJsonParser::new(text);
    assert_eq!(slice.next_event().unwrap(), JsonEvent::StartArray);
    let state = slice.state();
    let mut resumed = SliceJsonParser::resume(text, state);
    assert!(matches!(
        resumed.next_event().unwrap(),
        JsonEvent::String(std::borrow::Cow::Borrowed("plain"))
    ));
    let escaped = resumed.next_event().unwrap();
    assert!(matches!(
        escaped,
        JsonEvent::String(std::borrow::Cow::Owned(_))
    ));
    assert_eq!(escaped.into_owned(), JsonEvent::String("a\nb".into()));
    let mut reader = nrese_json::ReaderJsonParser::new(text.as_bytes());
    assert_eq!(reader.next_event().unwrap(), JsonEvent::StartArray);
    let saved = reader.next_event().unwrap().into_owned();
    assert_eq!(
        reader.next_event().unwrap(),
        JsonEvent::String("a\nb".into())
    );
    assert_eq!(saved, JsonEvent::String("plain".into()));
}

fn parse(
    body: &[u8],
    cap: usize,
    budget: &Arc<Budget>,
) -> Result<ServiceResults, QueryEvaluationError> {
    ServiceResults::from_slice(
        QueryResultsFormat::Json,
        body,
        cap,
        budget,
        &StopSource::none(),
    )
}

#[test]
fn n7_json_answers_undef_recursive_terms_and_metadata_escape() {
    let binding = r#"{"x":{"type":"triple","value":{"subject":{"type":"bnode","value":"a"},"predicate":{"type":"uri","value":"urn:p"},"object":{"type":"literal","value":"a\n\u20ac","xml:lang":"DE"}}},"y":{"type":"literal","value":"v","datatype":"http://www.w3.org/2001/XMLSchema#string"}}"#;
    for head_last in [false, true] {
        let head = r#""head":{"vars":["x","y","u"]}"#;
        let results = format!(r#""results":{{"bindings":[{binding},{{}},{binding}]}}"#);
        let body = if head_last {
            format!("{{{results},{head}}}")
        } else {
            format!("{{{head},{results}}}")
        };
        let budget = Arc::new(Budget::new(1 << 20));
        let answer = parse(body.as_bytes(), 3, &budget).unwrap();
        let old = ServiceResults::from_slice_copying(
            QueryResultsFormat::Json,
            body.as_bytes(),
            3,
            &Arc::new(Budget::unlimited()),
            &StopSource::none(),
        )
        .unwrap();
        assert_eq!(answer, old);
        assert_eq!(answer.rows().len(), 3);
        assert!(answer.rows()[0][2].is_none());
        assert!(answer.rows()[1].iter().all(Option::is_none));
        assert_eq!(answer.rows()[0], answer.rows()[2]);
        assert!(budget.used() > 0);
        let vars = answer.into_variables();
        assert_eq!(vars[0].as_str(), "x");
        assert_eq!(
            budget.used(),
            0,
            "escaping metadata deliberately has no row lease"
        );
        assert_eq!(vars[2].as_str(), "u");
    }
}

#[test]
fn n7_json_cap_at_binding_boundary_exact_eof_and_zero() {
    let budget = Arc::new(Budget::new(1 << 20));
    for head_last in [false, true] {
        let wrap = |rows: &str| {
            if head_last {
                format!(r#"{{"results":{{"bindings":[{rows}]}},"head":{{"vars":[]}}}}"#)
            } else {
                format!(r#"{{"head":{{"vars":[]}},"results":{{"bindings":[{rows}]}}}}"#)
            }
        };
        assert!(parse(wrap("").as_bytes(), 0, &budget).is_ok());
        assert!(parse(wrap("{}").as_bytes(), 1, &budget).is_ok());
        let error = parse(
            wrap(r#"{}, {"unknown":{"value":123}}"#).as_bytes(),
            1,
            &budget,
        )
        .unwrap_err();
        assert!(error.to_string().contains("more than 1 rows"), "{error}");
        assert!(
            parse(wrap("{}").as_bytes(), 0, &budget)
                .unwrap_err()
                .to_string()
                .contains("more than 0 rows")
        );
        assert!(parse(format!("{} trailing", wrap("{}")).as_bytes(), 1, &budget).is_err());
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn n7_json_native_memory_cancel_and_partial_result_cleanup() {
    let body = format!(
        r#"{{"head":{{"vars":["x"]}},"results":{{"bindings":[{{"x":{{"type":"literal","value":"{}"}}}}]}}}}"#,
        "z".repeat(20_000)
    );
    for limit in [0, 4, 256, 1 << 20] {
        let budget = Arc::new(Budget::new(limit));
        let answer = parse(body.as_bytes(), 1, &budget);
        if limit == 1 << 20 {
            assert!(answer.is_ok());
        } else {
            assert!(matches!(answer, Err(QueryEvaluationError::MemoryLimit(_))));
        }
        drop(answer);
        assert_eq!(budget.used(), 0);
    }
    struct Cancel(Cell<usize>);
    impl StopCheck for Cancel {
        fn check(&self) -> Result<(), Stop> {
            let n = self.0.get();
            self.0.set(n + 1);
            if n == 4 { Err(Stop::Cancelled) } else { Ok(()) }
        }
    }
    let budget = Arc::new(Budget::new(1 << 20));
    let answer = ServiceResults::from_slice(
        QueryResultsFormat::Json,
        body.as_bytes(),
        1,
        &budget,
        &Cancel(Cell::new(0)),
    );
    assert!(matches!(answer, Err(QueryEvaluationError::Cancelled)));
    assert_eq!(budget.used(), 0);
}

struct CallbackOwner {
    // Payload before its original reservation, for normal return and unwinding.
    kept: Option<Vec<Option<Term>>>,
    lease: Reservation,
    mode: u8,
    rows: usize,
}
fn callback_hooks() -> SolutionsCallbacks<CallbackOwner> {
    SolutionsCallbacks {
        reserve: |c, n| c.lease.charge(n).map_err(io::Error::other),
        free: |c, n| c.lease.release(n),
        check: |_| Ok(()),
        variables: |_, _| Ok(()),
        row: |c, row| {
            c.rows += 1;
            if c.rows == 1 {
                c.kept = Some(row);
                return Ok(());
            }
            drop(row); // Failed callback owns the incoming payload, before its credit.
            if c.mode == 1 {
                panic!("row callback unwind")
            }
            Err(io::Error::other(Marker))
        },
    }
}

#[test]
fn n7_json_callback_error_and_panic_keep_transferred_charge() {
    let body = br#"{"results":{"bindings":[{"x":{"type":"literal","value":"first"}},{"x":{"type":"literal","value":"second"}}]},"head":{"vars":["x"]}}"#;
    for mode in [0, 1] {
        let budget = Arc::new(Budget::new(1 << 20));
        let mut owner = CallbackOwner {
            kept: None,
            lease: Reservation::new(Arc::clone(&budget)),
            mode,
            rows: 0,
        };
        let answer = catch_unwind(AssertUnwindSafe(|| {
            QueryResultsParser::from_format(QueryResultsFormat::Json).read_solutions_with(
                body,
                2,
                &mut owner,
                &callback_hooks(),
            )
        }));
        if mode == 1 {
            assert!(answer.is_err());
        } else {
            assert!(
                matches!(answer.unwrap(), Err(QueryResultsParseError::Io(ref e)) if e.kind()==io::ErrorKind::Other && e.get_ref().is_some_and(|e|e.is::<Marker>()))
            );
        }
        assert_eq!(owner.rows, 2);
        assert!(owner.kept.is_some());
        assert!(budget.used() >= size_of::<Option<Term>>() + 5);
        drop(owner);
        assert_eq!(budget.used(), 0);
    }
}
