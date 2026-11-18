//! What a reader by support is told of a property chain its own graphs close, when a graph
//! it can't read equates the chain's properties: never `complete` with an answer the reader
//! of every graph contradicts.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::{QlRewritingMode, StoreConfig};
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::support::{body_text, test_app_with_store_config, test_state_with_store_config};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PREFIXES: &str = "PREFIX ex: <urn:ex:> PREFIX : <urn:graph:>
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";
const POLICY: &str = r#"
default = "deny"
inferred = "supported"

[[role]]
name = "one"
read = ["urn:graph:g1"]
default_graph = "read"
"#;
const READER: &[&str] = &["nrese.read", "one"];
const ADMIN: &[&str] = &["nrese.admin"];
const ASK: &str = "ASK { ex:x0 ex:q ex:x7 }";
const REMATERIALISE: &str = "/api/v1/repositories/nrese/reasoning/rematerialise";

/// `body` by `roles` to `uri` with `method`: an update to the update endpoint, a query to
/// the query endpoint, nothing to any other.
async fn send(
    app: &Router,
    roles: &[&str],
    method: Method,
    uri: &str,
    body: String,
) -> TestResult<(StatusCode, Option<String>, String)> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::ACCEPT, "application/sparql-results+json")
        .header(header::AUTHORIZATION, format!("Bearer {}", token(roles)));
    let body = match uri {
        "/dataset/update" => {
            request = request.header(header::CONTENT_TYPE, "application/sparql-update");
            Body::from(format!("{PREFIXES} {body}"))
        }
        u if u.starts_with("/dataset/query") => {
            request = request.header(header::CONTENT_TYPE, "application/sparql-query");
            Body::from(format!("{PREFIXES} {body}"))
        }
        _ => Body::empty(),
    };
    let response = app.clone().oneshot(request.body(body)?).await?;
    let completeness = response
        .headers()
        .get("nrese-completeness")
        .map(|v| v.to_str().map(str::to_owned))
        .transpose()?;
    Ok((response.status(), completeness, body_text(response).await?))
}

async fn insert(app: &Router, data: &str) -> TestResult {
    let update = format!("INSERT DATA {{ {data} }}");
    let (code, _, body) = send(app, ADMIN, Method::POST, "/dataset/update", update).await?;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    Ok(())
}

/// What `roles` are told of `ask`: the answer, the whole header, EXPLAIN's `ql` (`null`
/// where it has none) and its `completeness`'s status, with EXPLAIN's `completeness` printed
/// as `SEEN` under `label`.
async fn told(
    app: &Router,
    roles: &[&str],
    ask: &str,
    label: &str,
) -> TestResult<(bool, String, String, String)> {
    let (code, header, body) =
        send(app, roles, Method::POST, "/dataset/query", ask.to_owned()).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    let answer = serde_json::from_str::<serde_json::Value>(&body)?["boolean"]
        .as_bool()
        .ok_or("no boolean")?;
    let uri = "/dataset/query?explain=true";
    let (code, _, body) = send(app, roles, Method::POST, uri, ask.to_owned()).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    let explanation: serde_json::Value = serde_json::from_str(&body)?;
    let member = |name: &str| {
        explanation
            .get(name)
            .map_or("absent".to_owned(), ToString::to_string)
    };
    let header = header.unwrap_or_default();
    println!(
        "SEEN {label}: answer {answer}, header {header:?}, explain completeness {}, ql {}",
        member("completeness"),
        member("ql")
    );
    let explained = explanation["completeness"]["status"]
        .as_str()
        .unwrap_or("absent")
        .to_owned();
    Ok((answer, header, member("ql"), explained))
}

/// `ex:q owl:propertyChainAxiom (ex:p1 … ex:p7)` on blank cells (as the OWL mapping reads a
/// list), and the chain `ex:x0 ex:p1 ex:x1 … ex:x6 ex:p7 ex:x7`, in `:g1`.
fn chain() -> String {
    let mut data = "ex:q owl:propertyChainAxiom _:l1 .".to_owned();
    for i in 1..=7 {
        let rest = match i {
            7 => "rdf:nil".to_owned(),
            i => format!("_:l{}", i + 1),
        };
        data += &format!(
            " _:l{i} rdf:first ex:p{i} . _:l{i} rdf:rest {rest} . ex:x{} ex:p{i} ex:x{i} .",
            i - 1
        );
    }
    format!("GRAPH :g1 {{ {data} }}")
}

/// Spec: docs/spec/reasoning-semantics.md: `complete` says no entailed row is missing; a
/// reader by support reads the closure of the graphs it reads (crates/nrese-store/src/
/// support.rs, module docs). Under `owl2-rl`, `representatives` and the QL rewriting on, with
/// the default graph the union of every graph (a query with no dataset reads `:g1`), `:g1`
/// holds a seven-property chain axiom and a chain it closes. Revision B adds
/// `ex:pi owl:sameAs ex:ai` (i = 1…7) in `:g2`, which the reader by support can't read; B is
/// read after the commit and again after a rematerialisation. The reader of every graph
/// answers `ASK { ex:x0 ex:q ex:x7 }` `true` throughout; the reader by support, as it was
/// answered before its claim was withheld, `true` in A and `false` in B. In B the support
/// grounding leaves the chain's list out, so the reader's view isn't vouched for: it is told
/// exactly `not-certified`, in the header and EXPLAIN's `completeness`, and EXPLAIN's `ql`
/// is `null`. Each header and EXPLAIN's `completeness` and `ql` are printed (`SEEN …`).
#[tokio::test]
async fn a_reader_by_support_missing_a_chain_its_graphs_close_is_never_told_complete() -> TestResult
{
    let app = test_app_with_store_config(
        StoreConfig {
            ql_rewriting: QlRewritingMode::On,
            union_default_graph: true,
            equality_by_representatives: true,
            equality_compact: false,
            ..StoreConfig::default()
        },
        policy_of(POLICY)?,
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;
    insert(&app, &chain()).await?;
    let mut seen = vec![(
        "A",
        told(&app, ADMIN, ASK, "A every graph").await?,
        told(&app, READER, ASK, "A reader").await?,
    )];
    let same: String = (1..=7)
        .map(|i| format!(" ex:p{i} owl:sameAs ex:a{i} ."))
        .collect();
    insert(&app, &format!("GRAPH :g2 {{ {same} }}")).await?;
    seen.push((
        "B (commit)",
        told(&app, ADMIN, ASK, "B (commit) every graph").await?,
        told(&app, READER, ASK, "B (commit) reader").await?,
    ));
    let (code, _, body) = send(&app, ADMIN, Method::POST, REMATERIALISE, String::new()).await?;
    assert_eq!(code, StatusCode::OK, "{body}");
    seen.push((
        "B (rematerialised)",
        told(&app, ADMIN, ASK, "B (rematerialised) every graph").await?,
        told(&app, READER, ASK, "B (rematerialised) reader").await?,
    ));
    let mut wrong = Vec::new();
    for (at, whole, reader) in &seen {
        let in_b = at.starts_with('B');
        let withheld = (reader.1.as_str(), reader.2.as_str(), reader.3.as_str())
            == ("not-certified", "null", "not-certified");
        if !whole.0 || reader.0 == in_b || (in_b && !withheld) {
            wrong.push(format!("{at}: every graph {}, reader {reader:?}", whole.0));
        }
    }
    assert!(wrong.is_empty(), "wrong rows:\n{}", wrong.join("\n"));
    Ok(())
}

/// Spec: docs/spec/reasoning-semantics.md (access): "adding or removing an assertion the
/// reader can't see changes none of its answers (its status may change)". Under `owl2-rl`,
/// `representatives`, the QL rewriting on and the default graph the union of every graph,
/// `:g1` holds `ex:x a ex:A` and `ex:A rdfs:subClassOf [ owl:onProperty ex:p ;
/// owl:someValuesFrom ex:B ]`, which entail `ASK { ex:x ex:p ?y }`. Store 2 also holds, in
/// `:g2`, which a reader by support of `:g1` can't read, a chain axiom on a list with no
/// `rdf:rest`. That reader answers `true` in both stores: its status is store 1's as before
/// (`complete; regime=owl2-rl`), and `not-certified` in store 2, whose support grounding
/// leaves a list out (a claim taken away, never an answer). Each header and EXPLAIN's
/// `completeness` and `ql` are printed (`SEEN …`).
#[tokio::test]
async fn a_list_left_out_in_a_graph_the_reader_cant_read_changes_none_of_its_answers() -> TestResult
{
    let readable = "GRAPH :g1 { ex:x a ex:A . ex:p a owl:ObjectProperty . ex:A rdfs:subClassOf \
                    [ a owl:Restriction ; owl:onProperty ex:p ; owl:someValuesFrom ex:B ] }";
    let hidden = "GRAPH :g2 { ex:q owl:propertyChainAxiom _:l . _:l rdf:first ex:r }";
    let mut answers = Vec::new();
    for (store, data) in [
        ("store 1", readable.to_owned()),
        ("store 2", format!("{readable} {hidden}")),
    ] {
        let app = test_app_with_store_config(
            StoreConfig {
                ql_rewriting: QlRewritingMode::On,
                union_default_graph: true,
                equality_by_representatives: true,
                equality_compact: false,
                ..StoreConfig::default()
            },
            policy_of(POLICY)?,
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
        )?;
        insert(&app, &data).await?;
        let ask = "ASK { ex:x ex:p ?y }";
        let (answer, header, ..) = told(&app, READER, ask, &format!("{store} reader")).await?;
        answers.push(format!("{store}: {answer}, {header}"));
    }
    let expected = [
        "store 1: true, complete; regime=owl2-rl",
        "store 2: true, not-certified",
    ];
    assert_eq!(answers, expected, "what the reader is told");
    Ok(())
}

/// A reader of `:g1` and the default graph who sees every inference.
const VISIBLE_POLICY: &str = r#"
default = "deny"
inferred = "visible"

[[role]]
name = "one"
read = ["urn:graph:g1"]
default_graph = "read"
"#;

/// Spec: crates/nrese-store/src/service.rs (`status_for_its_reader`): a reader of some graphs'
/// status token "tells nothing of hidden data". Under `owl2-rl`, `reasoner.equality = "compact"`, the
/// QL rewriting off and the default graph the union of every graph, `:g1` holds
/// `ex:a ex:p ex:b`; store 2 also holds it in `:g2`, which a reader of `:g1` who sees every
/// inference can't read. That reader asking `ASK { ex:a ex:p ex:b }` is answered `true` and
/// told the same in both stores: `not-certified` (under compact no view is its closure), in
/// the header and EXPLAIN's `completeness`, with the same EXPLAIN `ql`.
#[tokio::test]
async fn a_compact_visible_reader_is_told_the_same_whether_a_graph_it_cant_read_holds_anything()
-> TestResult {
    let readable = "GRAPH :g1 { ex:a ex:p ex:b }";
    let mut seen = Vec::new();
    for (store, data) in [
        ("store 1", readable.to_owned()),
        (
            "store 2",
            format!("{readable} GRAPH :g2 {{ ex:a ex:p ex:b }}"),
        ),
    ] {
        let app = test_app_with_store_config(
            StoreConfig {
                ql_rewriting: QlRewritingMode::Off,
                union_default_graph: true,
                equality_compact: true,
                ..StoreConfig::default()
            },
            policy_of(VISIBLE_POLICY)?,
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
        )?;
        insert(&app, &data).await?;
        let ask = "ASK { ex:a ex:p ex:b }";
        seen.push(told(&app, READER, ask, &format!("{store} visible reader")).await?);
    }
    for told in &seen {
        let (answer, header, _, explained) = told;
        assert_eq!(
            (*answer, header.as_str(), explained.as_str()),
            (true, "not-certified", "not-certified"),
            "store 1, then store 2: {seen:?}"
        );
    }
    assert_eq!(seen[0], seen[1], "store 1, then store 2");
    Ok(())
}

/// The result cache's whole answers, the work reads did (parts stored, rejected and
/// computed), and its hits.
fn cache_work(store: &nrese_store::StoreService) -> (usize, (u64, u64, u64), u64) {
    let stats = store.query_cache_stats();
    (
        stats.answers,
        (stats.stored, stats.rejected, stats.misses),
        stats.hits,
    )
}

/// Each of `roles` sends `select` in turn: what it is told (its header, `none` where it has
/// none, and its answer's rows of `urn:ex:o`), and whether its read was a whole-output
/// admission (`fresh`: a new whole answer stored) or a whole-output hit (no part stored,
/// rejected or computed, no new whole answer, a hit).
async fn cached_reads(
    app: &Router,
    store: &nrese_store::StoreService,
    reads: &[(&[&str], bool)],
    select: &str,
) -> TestResult<Vec<(String, usize, bool)>> {
    let mut told = Vec::new();
    for (roles, fresh) in reads {
        let before = cache_work(store);
        let (code, header, body) = send(
            app,
            roles,
            Method::POST,
            "/dataset/query",
            select.to_owned(),
        )
        .await?;
        assert_eq!(code, StatusCode::OK, "{body}");
        let after = cache_work(store);
        let cache = match fresh {
            true => after.0 > before.0,
            false => after.0 == before.0 && after.1 == before.1 && after.2 > before.2,
        };
        let header = header.unwrap_or_else(|| "none".to_owned());
        told.push((header, body.matches("urn:ex:o").count(), cache));
    }
    Ok(told)
}

/// Whether `header` says the fixture's `owl2-rl` status: `sound-only`, regime `owl2-rl`.
fn owl2_rl(header: &str) -> bool {
    header
        .split("; ")
        .take(2)
        .eq(["sound-only", "regime=owl2-rl"])
}

/// Spec: crates/nrese-store/src/service.rs (`status_for_its_reader`) and
/// crates/nrese-server/src/http/sparql.rs (a cached answer's header): under compact equality
/// a reader of `:g1` who sees every inference reads the publication, so its answers and the
/// reader of every graph's share the store's result cache, each under its own key. Each is
/// told its own rows and status with an answer from it, whichever of them read first: its
/// first read admits its whole answer, its second is answered from it with no work. The
/// reader of every graph is answered 501 rows, told the fixture's `owl2-rl` status both
/// times; the other the 500 of `:g1`, told `not-certified`. `:g2`, which it can't read, holds
/// the 501st.
#[tokio::test]
async fn a_cached_answer_is_told_with_its_own_readers_status() -> TestResult {
    let facts: String = (0..500)
        .map(|i| format!("ex:s{i} ex:p ex:o{i} . "))
        .collect();
    let select = "SELECT ?s ?o WHERE { ?s ex:p ?o }";
    for (first, second) in [(ADMIN, READER), (READER, ADMIN)] {
        let state = test_state_with_store_config(
            StoreConfig {
                ql_rewriting: QlRewritingMode::Off,
                union_default_graph: true,
                equality_compact: true,
                ..StoreConfig::default()
            },
            policy_of(VISIBLE_POLICY)?,
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
        )?;
        let app = nrese_server::build_app(state.clone());
        insert(
            &app,
            &format!("GRAPH :g1 {{ {facts} }} GRAPH :g2 {{ ex:s500 ex:p ex:o500 }}"),
        )
        .await?;
        let reads = [
            (first, true),
            (second, true),
            (first, false),
            (second, false),
        ];
        let told = cached_reads(&app, &state.store(), &reads, select).await?;
        let every: Vec<_> = (0..4).filter(|i| reads[*i].0 == ADMIN).collect();
        let certified = &told[every[0]].0;
        let own = |i: usize| match reads[i].0 == ADMIN {
            true => (&told[i].0, told[i].1) == (certified, 501),
            false => (told[i].0.as_str(), told[i].1) == ("not-certified", 500),
        };
        let right = owl2_rl(certified) && (0..4).all(|i| own(i) && told[i].2);
        assert!(right, "first {first:?}: {told:?}");
    }
    Ok(())
}

/// Spec: crates/nrese-store/src/service.rs (`status_outside_dl`'s cache context): a cached
/// answer is kept under the status its read was assessed with, so a change of the
/// assessment that leaves the publication as it was reaches the next read. On a store
/// without reasoning, a reader of every graph's SELECT of 500 facts in the default graph is
/// told `complete`, its second read a whole-output hit; with the rules registered, the same
/// SELECT is read again: it is told `not-certified` (an inferred stack that isn't the rules'
/// closure), a new whole answer, and its repeat a hit told the same.
#[tokio::test]
async fn a_cached_answer_is_told_the_assessment_after_the_rules_are_registered() -> TestResult {
    let facts: String = (0..500)
        .map(|i| format!("ex:s{i} ex:p ex:o{i} . "))
        .collect();
    let select = "SELECT ?s ?o WHERE { ?s ex:p ?o }";
    let state = test_state_with_store_config(
        StoreConfig::default(),
        policy_of(VISIBLE_POLICY)?,
        ReasonerConfig::default(),
    )?;
    let app = nrese_server::build_app(state.clone());
    insert(&app, &facts).await?;
    let reads = [(ADMIN, true), (ADMIN, false)];
    let mut told = cached_reads(&app, &state.store(), &reads, select).await?;
    let owl2_rl = nrese_reasoner::rulesets::Ruleset::Owl2Rl;
    state.store().use_reasoning_rules(Some(owl2_rl.into()));
    told.extend(cached_reads(&app, &state.store(), &reads, select).await?);
    // Each read is told its header and 500 rows, and is the admission or the hit it reads as.
    let expected = [
        ("complete".to_owned(), 500, true),
        ("complete".to_owned(), 500, true),
        ("not-certified".to_owned(), 500, true),
        ("not-certified".to_owned(), 500, true),
    ];
    assert_eq!(told, expected);
    Ok(())
}

/// The reader of the pending rows: it reads and writes `:g1` and reads the default graph.
const WRITER_POLICY: &str = r#"
default = "deny"
inferred = "supported"

[[role]]
name = "one"
read = ["urn:graph:g1"]
write = ["urn:graph:g1"]
default_graph = "read"
"#;

/// What `roles` are told of each of `asks` inside an RDF4J transaction with `update`
/// pending: its status and its answer, each read with HTTP 200.
async fn pending_reads(
    app: &Router,
    roles: &[&str],
    update: &str,
    asks: &[&str],
) -> TestResult<Vec<(String, bool)>> {
    let bearer = format!("Bearer {}", token(roles));
    let begin = Request::builder()
        .method(Method::POST)
        .uri("/repositories/nrese/transactions")
        .header(header::AUTHORIZATION, &bearer)
        .body(Body::empty())?;
    let begin = app.clone().oneshot(begin).await?;
    assert_eq!(begin.status(), StatusCode::CREATED);
    let tx = begin.headers()[header::LOCATION].to_str()?.to_owned();
    let form = serde_urlencoded::to_string([("update", format!("{PREFIXES} {update}"))])?;
    let request = Request::builder()
        .method(Method::PUT)
        .uri(format!("{tx}?action=UPDATE"))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::AUTHORIZATION, &bearer)
        .body(Body::from(form))?;
    let response = app.clone().oneshot(request).await?;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        body_text(response).await?
    );
    let mut read = Vec::new();
    for ask in asks {
        let query = serde_urlencoded::to_string([("query", format!("{PREFIXES} {ask}"))])?;
        let request = Request::builder()
            .method(Method::PUT)
            .uri(format!("{tx}?action=QUERY&{query}"))
            .header(header::ACCEPT, "application/sparql-results+json")
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())?;
        let response = app.clone().oneshot(request).await?;
        let code = response.status();
        let header = response.headers().get("nrese-completeness").cloned();
        let body = body_text(response).await?;
        assert_eq!(code, StatusCode::OK, "{body}");
        let header = header.ok_or_else(|| format!("no status: {body}"))?;
        let answer = serde_json::from_str::<serde_json::Value>(&body)?["boolean"]
            .as_bool()
            .ok_or("no boolean")?;
        read.push((header.to_str()?.to_owned(), answer));
    }
    Ok(read)
}

/// Spec: docs/spec/reasoning-semantics.md (transactions): a read inside a transaction whose
/// pending operations may delete is `unsound`, as they may delete what an inference rests
/// on, when its view is certified; under compact equality a reader by support's view isn't
/// its closure, so no certificate stands and it is `not-certified`, the pending reasons
/// absorbed. So a reader by support of `:g1` asking for `ex:a ex:q ex:b` (which `:g1`
/// entails) after a pending `DELETE DATA` of `ex:a ex:p ex:b` is told `unsound` with the
/// delete reason under `representatives`, and `not-certified` under compact; after a pending
/// insert of an unrelated statement, under compact, `not-certified`. Each read answers with
/// HTTP 200 and reads the pending operation: after the delete `ex:a ex:p ex:b` is gone from
/// `:g1`, after the insert `ex:c ex:r ex:d` is there.
#[tokio::test]
async fn a_pending_delete_is_unsound_on_a_certified_view_and_a_compact_pending_read_not_certified()
-> TestResult {
    let delete = "DELETE DATA { GRAPH :g1 { ex:a ex:p ex:b } }";
    let deleted = ("ASK { GRAPH :g1 { ex:a ex:p ex:b } }", false);
    let add = "INSERT DATA { GRAPH :g1 { ex:c ex:r ex:d } }";
    let added = ("ASK { GRAPH :g1 { ex:c ex:r ex:d } }", true);
    let rows = [
        ("representatives, pending delete", false, delete, deleted),
        ("compact, pending delete", true, delete, deleted),
        ("compact, pending insert", true, add, added),
    ];
    let mut wrong = Vec::new();
    for (name, compact, update, (effect, there)) in rows {
        let app = test_app_with_store_config(
            StoreConfig {
                equality_compact: compact,
                ql_rewriting: QlRewritingMode::On,
                ..StoreConfig::default()
            },
            policy_of(WRITER_POLICY)?,
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
        )?;
        let data = "GRAPH :g1 { ex:p <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> ex:q . \
                    ex:a ex:p ex:b }";
        insert(&app, data).await?;
        let asks = ["ASK { ex:a ex:q ex:b }", effect];
        let read = pending_reads(&app, READER, update, &asks).await?;
        println!("SEEN {name}: {read:?}");
        let status = &read[0].0;
        let right = match compact {
            false => status.starts_with("unsound;") && status.contains("may delete"),
            true => status == "not-certified",
        };
        if !right || read[1].1 != there {
            wrong.push(format!("{name}: {read:?}"));
        }
    }
    assert!(wrong.is_empty(), "wrong rows:\n{}", wrong.join("\n"));
    Ok(())
}
