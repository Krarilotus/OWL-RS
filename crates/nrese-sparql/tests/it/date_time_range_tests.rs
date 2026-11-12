//! FILTER comparisons of a variable with an `xsd:dateTime` constant narrow the scan that
//! binds it, and a count's walk of its keys (`native/ranges.rs`): to the inline dateTimes
//! within two local days of the constants, and the dictionary's typed literals. A narrowing
//! only prunes, so every value that passes must survive it, whatever the timezones.
//!
//! Expected answers come from the oracle in this file, on each value's fields as read here
//! from its lexical form, never from the engine. Each test says which order it uses:
//! - [`Order::Engine`], the comparison the engine implements: instants where both values
//!   have a timezone, local times where neither has; with a timezone on one side only, the
//!   partial order of XSD 1.0 Part 2 §3.2.7.4 (an order only where it holds whatever
//!   timezone, -14:00 to +14:00, the other side had; otherwise the comparison is an error)
//!   and `=` false.
//! - [`Order::XPath`], XPath's `op:dateTime-less-than` and `op:dateTime-equal` (Functions and
//!   Operators 3.1), which give a value without a timezone the implicit timezone.

use std::cmp::Ordering::{self, Equal, Greater, Less};

use nrese_engine::{Engine, EngineConfig, Snapshot};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, NamedNode, NamedNodeRef, Quad, Term};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";

/// A dateTime's value: its local date and time in milliseconds on one count, and its
/// timezone in minutes.
type Value = (i64, Option<i64>);

/// The value of the dateTime lexical form `YYYY…-MM-DDThh:mm:ss[.f…][Z|±hh:mm]`, read here.
/// `24:00:00` is the next day's midnight. Digits past the milliseconds are dropped: no value
/// here is within a millisecond of a constant.
fn value(lexical: &str) -> Value {
    let number = |digits: &str| -> i64 { digits.parse().expect(lexical) };
    let bytes = lexical.as_bytes();
    let n = bytes.len();
    let (rest, zone) = if let Some(rest) = lexical.strip_suffix('Z') {
        (rest, Some(0))
    } else if matches!(bytes[n - 6], b'+' | b'-') && bytes[n - 3] == b':' {
        let (rest, zone) = lexical.split_at(n - 6);
        let minutes = number(&zone[1..3]) * 60 + number(&zone[4..6]);
        (
            rest,
            Some(if zone.starts_with('-') {
                -minutes
            } else {
                minutes
            }),
        )
    } else {
        (lexical, None)
    };
    let (date, time) = rest.split_once('T').expect(lexical);
    let date: Vec<i64> = date.rsplitn(3, '-').map(number).collect();
    let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
    let clock: Vec<i64> = clock.split(':').map(number).collect();
    let millis = number(&format!("{fraction:0<3}")[..3]);
    let seconds = days(date[2], date[1], date[0]) * 86_400 + clock[0] * 3_600 + clock[1] * 60;
    ((seconds + clock[2]) * 1_000 + millis, zone)
}

/// Days from 0000-03-01 to `year-month-day` in the proleptic Gregorian calendar, as XSD
/// counts (H. Hinnant's `days_from_civil`).
fn days(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year
}

#[derive(Clone, Copy)]
enum Order {
    Engine,
    /// With this implicit timezone, in minutes.
    XPath(i64),
}

/// `a` against `b` under `order`; `None` where it gives no order.
fn compare(order: Order, a: Value, b: Value) -> Option<Ordering> {
    let instant = |(local, zone): Value, implicit: i64| local - zone.unwrap_or(implicit) * 60_000;
    match (order, a.1.is_some(), b.1.is_some()) {
        (Order::XPath(implicit), _, _) => Some(instant(a, implicit).cmp(&instant(b, implicit))),
        (Order::Engine, true, true) => Some(instant(a, 0).cmp(&instant(b, 0))),
        (Order::Engine, false, false) => Some(a.0.cmp(&b.0)),
        // A timezone on `a` only: an order where it holds for `b` at +14:00 and at -14:00.
        (Order::Engine, true, false) => {
            let early = instant(a, 0).cmp(&instant(b, 14 * 60));
            (early == instant(a, 0).cmp(&instant(b, -14 * 60))).then_some(early)
        }
        (Order::Engine, false, true) => compare(order, b, a).map(Ordering::reverse),
    }
}

/// Whether `value <op> constant` is true; no order (an error) is false.
fn passes(order: Order, value: Value, op: &str, constant: Value) -> bool {
    let ordering = compare(order, value, constant);
    match op {
        "=" => ordering == Some(Equal),
        "<" => ordering == Some(Less),
        "<=" => matches!(ordering, Some(Less | Equal)),
        ">" => ordering == Some(Greater),
        ">=" => matches!(ordering, Some(Greater | Equal)),
        _ => unreachable!("{op}"),
    }
}

/// A conjunction of bounds on `?d`, each `(op, constant)`: `?d <op> constant`.
type Conjunction = [(&'static str, &'static str)];

/// Whether `object` passes every conjunct `?d <op> constant`: a literal of another datatype
/// never does (comparing it with a dateTime is an error).
fn passes_all(order: Order, object: &Literal, conjuncts: &[(&str, &str)]) -> bool {
    object.datatype() == xsd::DATE_TIME
        && conjuncts
            .iter()
            .all(|&(op, constant)| passes(order, value(object.value()), op, value(constant)))
}

/// The FILTER of `conjuncts`, each `?d <op> constant`, or with `mirrored` `constant <op'> ?d`.
fn filter(conjuncts: &[(&str, &str)], mirrored: bool) -> String {
    let conjuncts: Vec<String> = conjuncts
        .iter()
        .map(|&(op, constant)| {
            let constant = format!("\"{constant}\"^^<{}>", xsd::DATE_TIME.as_str());
            if !mirrored {
                return format!("?d {op} {constant}");
            }
            let op = match op {
                "<" => ">",
                "<=" => ">=",
                ">" => "<",
                ">=" => "<=",
                op => op,
            };
            format!("{constant} {op} ?d")
        })
        .collect();
    conjuncts.join(" && ")
}

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

/// An engine holding `e:<subject> e:<predicate> <object>` for each triple.
fn engine(triples: impl IntoIterator<Item = (String, String, Literal)>) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (subject, predicate, object) in triples {
        let quad = Quad::new(
            ex(&subject),
            ex(&predicate),
            object,
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// What `SELECT ?p WHERE { ?p e:<predicate> ?d FILTER(<filter>) }` returns (its subjects,
/// sorted), and EXPLAIN's step that read the pattern (`range scan` or `scan`) with its rows.
fn select(snapshot: &Snapshot, predicate: &str, filter: &str) -> (Vec<String>, String, u64) {
    let text = format!("SELECT ?p WHERE {{ ?p <{EX}{predicate}> ?d FILTER({filter}) }}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let options = QueryOptions::default();
    let QueryResults::Solutions(solutions) = evaluate_query(snapshot, &query, &options).unwrap()
    else {
        panic!("{text}: solutions")
    };
    let mut subjects: Vec<String> = solutions
        .map(|s| s.unwrap().get("p").expect("bound").to_string())
        .collect();
    subjects.sort();
    let steps = explain_query(snapshot, &query, &options).unwrap().steps;
    let scan = steps
        .iter()
        .find(|s| s.operator == "range scan" || s.operator == "scan")
        .unwrap_or_else(|| panic!("{text}: no scan among {steps:#?}"));
    (subjects, scan.operator.clone(), scan.rows)
}

/// `SELECT (COUNT(*) AS ?n) WHERE { ?p e:at ?d FILTER(<filter>) }`'s count, and the keys its
/// `key count` walked (EXPLAIN), if it took that route.
fn count(snapshot: &Snapshot, filter: &str) -> (u64, Option<u64>) {
    let text = format!("SELECT (COUNT(*) AS ?n) WHERE {{ ?p <{EX}at> ?d FILTER({filter}) }}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let options = QueryOptions::default();
    let QueryResults::Solutions(mut solutions) =
        evaluate_query(snapshot, &query, &options).unwrap()
    else {
        panic!("{text}: solutions")
    };
    let solution = solutions.next().expect("one row").unwrap();
    let Some(Term::Literal(n)) = solution.get("n") else {
        panic!("{text}: a count")
    };
    let n = n.value().parse().unwrap();
    let steps = explain_query(snapshot, &query, &options).unwrap().steps;
    let walked = steps.iter().find(|s| s.operator == "key count");
    (n, walked.map(|s| s.rows))
}

/// The subjects `e:v<i>` of `indexes`, as SELECT gives them, sorted.
fn subjects(indexes: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut subjects: Vec<String> = indexes.into_iter().map(|s| format!("<{EX}{s}>")).collect();
    subjects.sort();
    subjects
}

/// Spec: design note 12 Nov, "After the doubt review" (CRD-01): both operands can be ±14 h
/// off UTC, so a value that passes a bound can lie up to 28 h of local time beyond it, and
/// the bound is widened by two local days. Values and constants here all have timezones,
/// where the engine's order and XPath's agree (instants): `2022-12-30T23:00:00-14:00` is
/// three hours after `2023-01-01T00:00:00+14:00` and passes `>=`;
/// `2024-01-03T00:00:00+14:00` is three hours before `2024-01-01T23:00:00-14:00` and passes
/// `<`. Answers worked out by hand from the instants in the comments, by the scan and by
/// the key count, each bound written either way round.
#[test]
fn zoned_values_more_than_a_day_past_a_zoned_bound_pass() {
    // Their instants, UTC.
    let values = [
        "2022-12-30T19:59:59.999-14:00", // 0: 2022-12-31T09:59:59.999
        "2022-12-30T20:00:00-14:00",     // 1: 2022-12-31T10:00
        "2022-12-30T23:00:00-14:00",     // 2: 2022-12-31T13:00
        "2022-12-31T10:00:00Z",          // 3: 2022-12-31T10:00
        "2023-01-01T00:00:00+14:00",     // 4: 2022-12-31T10:00
        "2024-01-02T12:59:59Z",          // 5: 2024-01-02T12:59:59
        "2024-01-03T00:00:00+14:00",     // 6: 2024-01-02T10:00
        "2024-01-03T03:00:00+14:00",     // 7: 2024-01-02T13:00
        "2024-01-03T03:00:00.001+14:00", // 8: 2024-01-02T13:00:00.001
    ];
    let low = "2023-01-01T00:00:00+14:00"; // 2022-12-31T10:00
    let high = "2024-01-01T23:00:00-14:00"; // 2024-01-02T13:00
    let cases: [(&Conjunction, &[usize]); 7] = [
        (&[(">=", low)], &[1, 2, 3, 4, 5, 6, 7, 8]),
        (&[(">", low)], &[2, 5, 6, 7, 8]),
        (&[("=", low)], &[1, 3, 4]),
        (&[("<", high)], &[0, 1, 2, 3, 4, 5, 6]),
        (&[("<=", high)], &[0, 1, 2, 3, 4, 5, 6, 7]),
        (&[("=", high)], &[7]),
        (&[(">=", low), ("<", high)], &[1, 2, 3, 4, 5, 6]),
    ];
    let engine = engine(values.iter().enumerate().map(|(i, lexical)| {
        let object = Literal::new_typed_literal(*lexical, xsd::DATE_TIME);
        (format!("v{i}"), "at".to_owned(), object)
    }));
    let snapshot = engine.snapshot();
    for (conjuncts, passing) in cases {
        let expected = subjects(passing.iter().map(|i| format!("v{i}")));
        for mirrored in [false, true] {
            let filter = filter(conjuncts, mirrored);
            assert_eq!(select(&snapshot, "at", &filter).0, expected, "{filter}");
            assert_eq!(
                count(&snapshot, &filter).0,
                passing.len() as u64,
                "{filter}"
            );
        }
    }
}

/// Typed literals the dictionary holds: dateTimes in no canonical form, and a gYear.
const TYPED: [(&str, NamedNodeRef<'static>); 3] = [
    ("2023-05-05T24:00:00", xsd::DATE_TIME),
    ("2023-05-05T10:00:00.100", xsd::DATE_TIME),
    ("2023", xsd::G_YEAR),
];

/// Data as perf_lab's social `created` has it, minute-resolved local dateTimes without a
/// timezone in 2020-2025, on days 1, 10, 19 and 28 of each month; then [`TYPED`] and other
/// kinds (a date, an integer, a string). Each object is held by two subjects, `e:v<i>a` and
/// `e:v<i>b`. With the objects, and how many of them come first as inline dateTimes.
fn years() -> (Engine, Vec<Literal>, usize) {
    let mut objects = Vec::new();
    for year in 2020..=2025 {
        for month in 1..=12 {
            for day in [1, 10, 19, 28] {
                let (hour, minute) = (objects.len() * 7 % 24, objects.len() * 13 % 60);
                let lexical = format!("{year}-{month:02}-{day:02}T{hour:02}:{minute:02}:00");
                objects.push(Literal::new_typed_literal(lexical, xsd::DATE_TIME));
            }
        }
    }
    let inline = objects.len();
    for (lexical, datatype) in TYPED {
        objects.push(Literal::new_typed_literal(lexical, datatype));
    }
    objects.push(Literal::new_typed_literal("2023-05-05", xsd::DATE));
    objects.push(Literal::new_typed_literal("2023", xsd::INTEGER));
    objects.push(Literal::new_simple_literal("2023-05-05T10:00:00"));
    let triples = objects.iter().enumerate().flat_map(|(i, object)| {
        ["a", "b"].map(|copy| (format!("v{i}{copy}"), "at".to_owned(), object.clone()))
    });
    (engine(triples), objects, inline)
}

/// The subjects of [`years`]' objects that pass `conjuncts` ([`Order::Engine`]).
fn years_passing(objects: &[Literal], conjuncts: &[(&str, &str)]) -> Vec<String> {
    subjects(
        (0..objects.len())
            .filter(|&i| passes_all(Order::Engine, &objects[i], conjuncts))
            .flat_map(|i| [format!("v{i}a"), format!("v{i}b")]),
    )
}

/// Spec: design note 12 Nov (the remedy, kept by "After the doubt review"): bounds on a
/// variable by inline dateTimes narrow the scan that binds it, and a count's walk of its
/// keys, to the inline dateTimes within two local days of the bounds plus the dictionary's
/// typed literals: created-2023's query reads about one year of six. The same bounds in
/// forms that aren't inline (`24:00:00`, a non-canonical fraction) give no hint: the whole
/// pattern is read. Answers: [`Order::Engine`]; without timezones it is XPath's too.
#[test]
fn date_time_bounds_narrow_the_scan_and_the_key_count() {
    let (engine, objects, inline) = years();
    let snapshot = engine.snapshot();
    let created_2023 = [(">=", "2023-01-01T00:00:00"), ("<", "2024-01-01T00:00:00")];
    let not_inline = [
        (">=", "2022-12-31T24:00:00"),
        ("<", "2024-01-01T00:00:00.000"),
    ];
    // What two days around the bounds hold: the inline dateTimes from 2022-12-30T00:00:00
    // to 2024-01-03T00:00:00, and every typed literal.
    let (from, to) = (
        value("2022-12-30T00:00:00").0,
        value("2024-01-03T00:00:00").0,
    );
    let near = (0..inline)
        .filter(|&i| (from..=to).contains(&value(objects[i].value()).0))
        .count();
    let envelope_keys = (near + TYPED.len()) as u64;
    let expected = years_passing(&objects, &created_2023);
    assert_eq!(years_passing(&objects, &not_inline), expected);
    let filter_2023 = filter(&created_2023, false);
    let (answer, scan, rows) = select(&snapshot, "at", &filter_2023);
    assert_eq!(answer, expected, "{filter_2023}");
    assert_eq!(scan, "range scan", "{filter_2023}");
    assert!(
        rows <= 2 * envelope_keys,
        "{filter_2023}: {rows} rows read, at most {} in two days",
        2 * envelope_keys
    );
    let (n, walked) = count(&snapshot, &filter_2023);
    assert_eq!(n, expected.len() as u64, "{filter_2023}");
    assert!(
        walked.is_some_and(|walked| walked <= envelope_keys),
        "{filter_2023}: {walked:?} keys walked, at most {envelope_keys} in two days"
    );
    let filter_not_inline = filter(&not_inline, false);
    let (answer, scan, rows) = select(&snapshot, "at", &filter_not_inline);
    assert_eq!(answer, expected, "{filter_not_inline}");
    let all = objects.len() as u64;
    assert_eq!(
        (scan.as_str(), rows),
        ("scan", 2 * all),
        "{filter_not_inline}"
    );
    assert_eq!(
        count(&snapshot, &filter_not_inline),
        (expected.len() as u64, Some(all)),
        "{filter_not_inline}"
    );
}

/// dateTimes at and around the constants of [`BOUNDS`]: inline ones with and without
/// timezones, at month ends, leap days and the inline years' ends, and ones the dictionary
/// holds (`24:00:00`, non-canonical fractions and offsets, a year past 9999).
const DATE_TIMES: [&str; 32] = [
    "2022-12-29T23:59:59",
    "2022-12-30T19:59:59.999-14:00",
    "2022-12-30T20:00:00-14:00",
    "2022-12-30T23:00:00-14:00",
    "2022-12-31T10:00:00Z",
    "2022-12-31T24:00:00",
    "2023-01-01T00:00:00",
    "2023-01-01T00:00:00.5",
    "2023-01-01T00:00:00.500",
    "2023-01-01T14:00:00.001Z",
    "2023-01-02T00:00:00-14:00",
    "2023-02-27T19:00:00-14:00",
    "2023-02-28T01:00:00-14:00",
    "2023-06-01T00:00:00+00:00",
    "2023-06-01T12:00:00+05:20",
    "2023-06-01T12:00:00.1234",
    "2023-12-31T23:59:59.999",
    "2024-01-01T00:00:00",
    "2024-01-02T13:00:00Z",
    "2024-01-03T00:00:00+14:00",
    "2024-01-03T02:59:59",
    "2024-01-03T03:00:00.001+14:00",
    "2024-02-29T00:00:00-14:00",
    "2024-02-29T24:00:00",
    "2024-03-01T02:00:00+14:00",
    "0000-01-01T00:00:00",
    "0000-01-01T00:00:00-14:00",
    "0000-01-01T23:00:00+14:00",
    "9999-12-30T23:00:00-14:00",
    "9999-12-31T23:59:59.999+14:00",
    "9999-12-31T23:59:59.999-14:00",
    "10000-01-01T00:00:00Z",
];

/// Conjunctions on `?d`.
const BOUNDS: [&Conjunction; 20] = [
    // The two zoned counterexamples (CRD-01), and equality at the first's instant.
    &[(">=", "2023-01-01T00:00:00+14:00")],
    &[("<", "2024-01-01T23:00:00-14:00")],
    &[("=", "2023-01-01T00:00:00+14:00")],
    // created-2023: no timezones.
    &[(">=", "2023-01-01T00:00:00"), ("<", "2024-01-01T00:00:00")],
    // A timezone on one side only.
    &[(">", "2023-01-01T00:00:00")],
    &[("<", "2023-01-01T00:00:00Z")],
    &[("<=", "2024-01-01T23:00:00-14:00")],
    // Fractions, and equal values in other forms.
    &[(">", "2023-01-01T00:00:00.5")],
    &[("<=", "2023-01-01T00:00:00.5")],
    &[("=", "2023-01-01T00:00:00.5")],
    &[("=", "2023-01-01T00:00:00")],
    // Constants that aren't inline: no hint.
    &[(">=", "2022-12-31T24:00:00")],
    &[("<", "2023-01-01T00:00:00.000")],
    // Month ends and leap days.
    &[(">=", "2023-03-01T05:00:00+14:00")],
    &[("<", "2024-02-28T23:00:00-14:00")],
    &[(">=", "2024-03-01T01:00:00+14:00")],
    // The inline years' ends.
    &[("<", "0000-01-01T10:00:00-14:00")],
    &[(">=", "0000-01-01T05:00:00+14:00")],
    &[(">", "9999-12-31T00:00:00+14:00")],
    &[("<=", "9999-12-31T10:00:00-14:00")],
];

/// Spec: design note 12 Nov, "After the doubt review": the narrowing keeps every value that
/// passes, whatever the timezones on either side, across month ends, leap days and the
/// inline years' ends; dateTimes that aren't inline (`24:00:00`, non-canonical fractions and
/// offsets, years past 9999) stay in the typed literals' range; a constant that isn't
/// inline narrows nothing; a dateTime bound never matches a date, an integer or a string.
/// Answers: [`Order::Engine`], by the scan and by the key count, each bound written either
/// way round.
#[test]
fn date_time_bounds_keep_every_value_that_passes() {
    let objects: Vec<Literal> = DATE_TIMES
        .iter()
        .map(|lexical| Literal::new_typed_literal(*lexical, xsd::DATE_TIME))
        .chain([
            Literal::new_typed_literal("2023-01-01", xsd::DATE),
            Literal::new_typed_literal("2024-01-01-14:00", xsd::DATE),
            Literal::new_typed_literal("2023", xsd::INTEGER),
            Literal::new_simple_literal("2023-01-01T00:00:00"),
        ])
        .collect();
    let engine = engine(
        objects
            .iter()
            .enumerate()
            .map(|(i, object)| (format!("v{i}"), "at".to_owned(), object.clone())),
    );
    let snapshot = engine.snapshot();
    for conjuncts in BOUNDS {
        let passing: Vec<usize> = (0..objects.len())
            .filter(|&i| passes_all(Order::Engine, &objects[i], conjuncts))
            .collect();
        let expected = subjects(passing.iter().map(|i| format!("v{i}")));
        for mirrored in [false, true] {
            let filter = filter(conjuncts, mirrored);
            assert_eq!(select(&snapshot, "at", &filter).0, expected, "{filter}");
            assert_eq!(
                count(&snapshot, &filter).0,
                passing.len() as u64,
                "{filter}"
            );
        }
    }
}

/// Spec: design note 12 Nov, "After the doubt review" (CRD-07): the two-day envelope also
/// covers XPath's comparison, where a value without a timezone takes the implicit one,
/// though the engine compares with XSD's partial order and its FILTER rejects some of those
/// values. Order: [`Order::XPath`], with the implicit timezones -14:00, Z and +14:00. Every
/// value one of them passes survives the narrowing: alone under a predicate of its own, the
/// scan of its pattern reads its row.
#[test]
fn every_value_xpath_passes_survives_the_narrowing() {
    let engine = engine(DATE_TIMES.iter().enumerate().map(|(i, lexical)| {
        let object = Literal::new_typed_literal(*lexical, xsd::DATE_TIME);
        (format!("v{i}"), format!("at{i}"), object)
    }));
    let snapshot = engine.snapshot();
    // Values checked, and those among them the engine's order rejects.
    let (mut checked, mut xpath_only) = (0, 0);
    for &(op, constant) in BOUNDS.iter().flat_map(|conjuncts| conjuncts.iter()) {
        let filter = filter(&[(op, constant)], false);
        for (i, lexical) in DATE_TIMES.iter().enumerate() {
            let (v, c) = (value(lexical), value(constant));
            let xpath = [-14 * 60, 0, 14 * 60]
                .into_iter()
                .any(|zone| passes(Order::XPath(zone), v, op, c));
            if xpath {
                let (_, scan, rows) = select(&snapshot, &format!("at{i}"), &filter);
                assert_eq!(rows, 1, "{lexical}: {filter}: its {scan} read nothing");
                checked += 1;
                xpath_only += usize::from(!passes(Order::Engine, v, op, c));
            }
        }
    }
    assert!(
        checked > 100 && xpath_only > 10,
        "{checked} checked, {xpath_only} XPath's only"
    );
}

/// Spec: brief of 12 Nov: a conjunction mixing a dateTime bound with a YEAR or an integer
/// bound on one variable keeps today's behaviour. Before dateTimes had a hint, such a
/// conjunct gave none, and the other bound's hint narrowed the scan and the key count
/// alone; so it does now, whichever conjunct comes first (two other kinds of bound on one
/// variable give no hint at all). Answers: [`Order::Engine`], with YEAR read from the
/// lexical form (these values have no timezone and no `24:00:00`).
#[test]
fn a_date_time_bound_beside_a_year_or_integer_bound_keeps_that_hint() {
    let (engine, objects, inline) = years();
    let snapshot = engine.snapshot();
    let june = "2023-06-01T00:00:00";
    let bound = |op: &str| filter(&[(op, june)], false);
    let after_june_2023 = subjects(
        (0..inline)
            .filter(|&i| {
                let lexical = objects[i].value();
                lexical.starts_with("2023-")
                    && passes(Order::Engine, value(lexical), ">=", value(june))
            })
            .flat_map(|i| [format!("v{i}a"), format!("v{i}b")]),
    );
    let cases = [
        (
            "YEAR(?d) = 2023",
            [
                format!("YEAR(?d) = 2023 && {}", bound(">=")),
                format!("{} && YEAR(?d) = 2023", bound(">=")),
            ],
            after_june_2023,
        ),
        (
            "?d > 5",
            [
                format!("?d > 5 && {}", bound("<")),
                format!("{} && ?d > 5", bound("<")),
            ],
            Vec::new(),
        ),
    ];
    for (alone, mixed, expected) in cases {
        let (_, scan, rows) = select(&snapshot, "at", alone);
        let (_, walked) = count(&snapshot, alone);
        assert_eq!(scan, "range scan", "{alone}");
        for filter in mixed {
            assert_eq!(
                select(&snapshot, "at", &filter),
                (expected.clone(), scan.clone(), rows),
                "{filter}"
            );
            assert_eq!(
                count(&snapshot, &filter),
                (expected.len() as u64, walked),
                "{filter}"
            );
        }
    }
}
