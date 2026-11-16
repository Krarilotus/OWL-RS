//! A read is told what the data it reads is certified for, and nothing more: `complete` or
//! `sound-only` only where the reasoning certified the snapshot it reads, and `not-certified`
//! where nothing did (docs/spec/reasoning-semantics.md, "What an answer promises"). A read
//! inside a transaction is told less than its base: the pending operations aren't reasoned
//! over. Each cell prints `CASE name: right …` or `CASE name: WRONG …`; a cell whose setup
//! didn't hold prints `UNMET`. Every expectation is the rule's, written by hand.

use std::path::Path;
use std::sync::Arc;

use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use nrese_sparql::GraphAccess;
use nrese_store::{
    BulkLoadRequest, CancellationToken, GraphTarget, MutationPipeline, QlRewritingMode, ReadScope,
    SparqlQueryRequest, SparqlUpdateRequest, StatementOp, StatementsRequest, StoreConfig,
    StoreService,
};

use crate::dl::{PREFIXES, insert, pipeline};
use crate::support::Writes;

/// A subclass and an instance of it, and an instance of `:C` for the negated read.
const DATA: &str = ":Dog rdfs:subClassOf :Animal . :rex a :Dog . :a a :C .";

/// A monotone read: each row it returns is entailed where the data is closed.
const READ: &str = "SELECT ?x { ?x a :Animal }";

/// A read under which a missing row can make a returned one wrong: with `:C rdfs:subClassOf
/// :D` pending, `:a a :D` is entailed but not yet inferred, so `:a` is returned and isn't a
/// certain answer.
const NEGATED: &str = "SELECT ?x { ?x a :C FILTER NOT EXISTS { ?x a :D } }";

/// The cells' verdicts.
#[derive(Default)]
struct Cells {
    wrong: Vec<String>,
    unmet: Vec<String>,
}

impl Cells {
    /// The cell `name` is right if `got`'s status (its first word: `complete`,
    /// `sound-only`, `unsound`, `not-certified`, or `none`) is `want`.
    fn class(&mut self, name: &str, got: &str, want: &str) {
        self.header(name, got, |h| class(h) == want, want);
    }

    /// The cell `name` is right if `got`, the whole status header, satisfies `right`.
    fn header(&mut self, name: &str, got: &str, right: impl Fn(&str) -> bool, want: &str) {
        if right(got) {
            println!("CASE {name}: right {got}");
        } else {
            println!("CASE {name}: WRONG {got}, want {want}");
            self.wrong.push(name.to_owned());
        }
    }

    /// The setup of `name` didn't hold: the cell says nothing.
    fn unmet(&mut self, name: &str, why: &str) {
        println!("UNMET {name}: {why}");
        self.unmet.push(name.to_owned());
    }

    fn finish(self) {
        assert!(
            self.wrong.is_empty() && self.unmet.is_empty(),
            "wrong: {:?}; unmet: {:?}",
            self.wrong,
            self.unmet
        );
    }
}

/// A status header's first word.
fn class(header: &str) -> &str {
    header.split(';').next().unwrap_or(header)
}

/// The status header of `status`, or `none`.
fn header(status: Option<nrese_sparql::Completeness>) -> String {
    status.map_or_else(|| "none".to_owned(), |s| s.header())
}

/// Loads `data` (Turtle over [`PREFIXES`]'s names), replacing the store's data or adding to it.
fn load(store: &StoreService, data: &str, replace: bool) {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("data.ttl");
    let prefixes = PREFIXES.replace("PREFIX", "@prefix").replace("> ", "> . ");
    std::fs::write(&file, format!("{prefixes}\n{data}")).expect("file");
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![file],
            replace,
            graph: GraphTarget::DefaultGraph,
            invalid: Some(nrese_store::InvalidStatements::Strict),
        })
        .expect("load");
}

/// A store holding [`DATA`], reasoned under `ruleset`.
fn certified(config: StoreConfig, ruleset: Ruleset) -> StoreService {
    let store = StoreService::new(config).expect("store");
    load(&store, DATA, false);
    store.rematerialise(ruleset).expect("reasoned");
    store
}

/// [`certified`] under OWL 2 RL as a server opens a store: its pipeline's reasoner
/// registers the rules before the data comes. A library store that rematerialises with no
/// rules registered reads a version without inferred statements as no reasoning at all.
fn served(config: StoreConfig) -> Arc<StoreService> {
    let store = Arc::new(StoreService::new(config).expect("store"));
    let _pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    load(&store, DATA, false);
    store.rematerialise(Ruleset::Owl2Rl).expect("reasoned");
    store
}

/// The status header of `query` (over [`PREFIXES`]).
fn status(store: &StoreService, query: &str) -> String {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}{query}"));
    header(
        store
            .execute_query(&request)
            .expect("answered")
            .completeness,
    )
}

/// The status headers EXPLAIN and the plan give `query`.
fn explained_and_planned(store: &StoreService, query: &str) -> [String; 2] {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}{query}"));
    let prepared = store.prepare_query(&request).expect("prepared");
    let cancel = CancellationToken::new();
    let explained = store.explain_query(&prepared, &cancel).expect("explained");
    let planned = store.plan_query(&prepared, &cancel).expect("planned");
    [header(explained.completeness), header(planned.completeness)]
}

/// An update of the transaction (over [`PREFIXES`]).
fn op(update: &str) -> StatementOp {
    StatementOp::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{update}")))
}

/// The status header of `query` read inside a transaction with `ops` pending.
fn pending(store: &StoreService, ops: Vec<StatementOp>, query: &str) -> String {
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!("{PREFIXES}{query}")))
        .expect("prepared");
    let (mut out, mut status) = (Vec::new(), None);
    store
        .run_query_pending_reporting(
            &StatementsRequest::new(ops),
            &prepared,
            &CancellationToken::new(),
            &mut out,
            |s| status = s,
        )
        .expect("answered");
    header(status)
}

/// A store holding `data` (over [`PREFIXES`]), certified under each regime: `rdfs`, user rules
/// (`custom`: a subclass rule and a transitive property's), `owl2-rl` with the QL rewriting
/// off and on, and `owl2-dl`.
fn regimes(data: &str) -> [(&'static str, StoreService); 5] {
    let ruleset = |ruleset: Ruleset, ql: QlRewritingMode| {
        let store = StoreService::new(StoreConfig {
            ql_rewriting: ql,
            ..StoreConfig::in_memory()
        })
        .expect("store");
        load(&store, data, false);
        store.rematerialise(ruleset).expect("reasoned");
        store
    };
    let dl = pipeline();
    insert(&dl, data).expect("data");
    let rules = UserRules::n3(
        "subclass.n3",
        "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .\n\
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n\
         @prefix owl: <http://www.w3.org/2002/07/owl#> .\n\
         { ?x rdf:type ?c . ?c rdfs:subClassOf ?d } => { ?x rdf:type ?d } .\n\
         { ?p rdf:type owl:TransitiveProperty . ?x ?p ?y . ?y ?p ?z } => { ?x ?p ?z } .\n",
    )
    .expect("the rules compile");
    let custom = MutationPipeline::new(
        Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store")),
        Arc::new(ReasonerService::new(
            ReasonerConfig::for_mode(ReasoningMode::Custom)
                .with_rules(Some(Arc::new(rules)))
                .expect("a mode for the rules"),
        )),
    );
    insert(&custom, data).expect("data");
    [
        ("rdfs", ruleset(Ruleset::Rdfs, QlRewritingMode::Off)),
        ("custom", custom.store().as_ref().clone()),
        (
            "owl2-rl, ql off",
            ruleset(Ruleset::Owl2Rl, QlRewritingMode::Off),
        ),
        (
            "owl2-rl, ql on",
            ruleset(Ruleset::Owl2Rl, QlRewritingMode::On),
        ),
        ("owl2-dl", dl.store().as_ref().clone()),
    ]
}

/// A read inside a transaction whose pending operations are all additions of statements
/// (`StatementOp::Add`, the route that knows nothing is deleted): the additions aren't
/// reasoned over, so an entailed row may be missing (`:x :p :z`, with `:p` transitive and
/// `:y :p :z` pending): `sound-only`, never `complete`, in every regime (30a over the
/// additions' own route). With N3 beside it as the guard: an addition under a negation is
/// `unsound`.
#[test]
fn a_read_inside_a_transaction_of_additions_alone_is_never_complete() {
    let mut cells = Cells::default();
    let read = "SELECT ?o { :x :p ?o }";
    let add = |triple: &str| {
        vec![StatementOp::Add {
            data: nrese_store::RdfPayload {
                payload: triple.as_bytes().to_vec().into(),
                format: nrese_store::GraphResultFormat::NTriples,
                base_iri: None,
            },
            contexts: Vec::new(),
        }]
    };
    let ex = |name: &str| format!("<http://example.com/{name}>");
    let transitive = || add(&format!("{} {} {} .", ex("y"), ex("p"), ex("z")));
    let subclass = || {
        add(&format!(
            "{} <http://www.w3.org/2000/01/rdf-schema#subClassOf> {} .",
            ex("C"),
            ex("D")
        ))
    };
    for (name, store) in &regimes(&format!("{DATA} :p a owl:TransitiveProperty . :x :p :y .")) {
        let base = status(store, read);
        if matches!(class(&base), "none" | "not-certified") {
            cells.unmet(name, &format!("the base isn't certified: {base}"));
            continue;
        }
        cells.class(
            &format!("30a additions alone | {name}"),
            &pending(store, transitive(), read),
            "sound-only",
        );
        cells.class(
            &format!("N3 an addition alone under a negation | {name}"),
            &pending(store, subclass(), NEGATED),
            "unsound",
        );
    }
    cells.finish();
}

/// The transaction's reads, over a certified base and over one that no certificate covers:
/// - additions only: never `complete`; the answers are entailed but some may be missing,
///   so `sound-only` (30a, 30e);
/// - a deletion among them: `unsound`, its inferences remain until the commit (30b);
/// - an addition under a negation (N3): `unsound`, whatever the route says alone, the QL
///   rewriting's `complete` included: a composition only takes away. The query's own
///   negation rule says so in every regime (R3), not only where the regime's helper does:
///   today the mark of a possible deletion, given for any pending operation, hides that;
///   once it is given only for a deletion, N3 is the guard;
/// - no certificate on the base: `not-certified`, the deletion's mark absorbed (30c, 30d).
///
/// Under `rdfs`, under user rules (`custom`, a subclass rule), under `owl2-rl` with the QL
/// rewriting off and on, and under `owl2-dl`.
#[test]
fn a_read_inside_a_transaction_is_told_no_more_than_its_base_allows() {
    let mut cells = Cells::default();
    let additions = || vec![op("INSERT DATA { :tom a :Dog }")];
    let deletion = || {
        vec![
            op("INSERT DATA { :tom a :Dog }"),
            op("DELETE DATA { :rex a :Dog }"),
        ]
    };
    let subclass = || vec![op("INSERT DATA { :C rdfs:subClassOf :D }")];
    for (name, store) in &regimes(DATA) {
        let base = status(store, READ);
        if matches!(class(&base), "none" | "not-certified") {
            cells.unmet(name, &format!("the base isn't certified: {base}"));
            continue;
        }
        cells.class(
            &format!("30a/30e additions | {name}"),
            &pending(store, additions(), READ),
            "sound-only",
        );
        cells.class(
            &format!("30b a deletion | {name}"),
            &pending(store, deletion(), READ),
            "unsound",
        );
        cells.class(
            &format!("N3 an addition under a negation | {name}"),
            &pending(store, subclass(), NEGATED),
            "unsound",
        );
        store.invalidate_reasoning().expect("invalidated");
        cells.class(
            &format!("30c additions, no certificate | {name}"),
            &pending(store, additions(), READ),
            "not-certified",
        );
        cells.class(
            &format!("30d a deletion, no certificate | {name}"),
            &pending(store, deletion(), READ),
            "not-certified",
        );
    }
    cells.finish();
}

/// Under `owl2-dl`, a question about an individual the data doesn't name, in an ontology with
/// a key, is told the keys' reason whatever reason came before it: reasons accumulate as the
/// status only weakens, so a user who clears the first one isn't then met by one they were
/// never told.
/// - Inside a transaction, after the pending operations' reason (the guard's cell).
/// - With a typed constant, after its reason (the order's: the negation rule and the
///   query's own reasons come after the mark).
#[test]
fn the_keys_reason_is_given_beside_every_earlier_reason() {
    let mut cells = Cells::default();
    let dl = pipeline();
    insert(
        &dl,
        ":r a owl:ObjectProperty . :A a owl:Class . :a a owl:NamedIndividual , :A . \
         owl:Thing owl:hasKey ( :r ) ; \
         rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:hasValue :a ] .",
    )
    .expect("the keys");
    let store = dl.store();
    let keys = "the ontology has keys";
    let both = |cells: &mut Cells, name: &str, got: &str, first: &str| {
        cells.header(
            name,
            got,
            |h| h.contains(first) && h.contains(keys),
            &format!("both \"{first}\" and \"{keys}\""),
        );
    };
    let add = vec![StatementOp::Add {
        data: nrese_store::RdfPayload {
            payload: b"<http://example.com/c> <http://example.com/note> \"c\" ."
                .to_vec()
                .into(),
            format: nrese_store::GraphResultFormat::NTriples,
            base_iri: None,
        },
        contexts: Vec::new(),
    }];
    both(
        &mut cells,
        "the keys' reason after the pending one",
        &pending(store, add, "ASK { :b a :A }"),
        "a read inside a transaction",
    );
    both(
        &mut cells,
        "the keys' reason after a typed constant's",
        &status(store, "ASK { :b a :A . :a :note \"1\"^^xsd:integer }"),
        "a stored value written another way",
    );
    cells.finish();
}

/// A store whose certificate was dropped (`invalidate_reasoning`) holds the old inferred
/// statements and no certificate for them: every reasoned read is `not-certified`, on every
/// path. With the QL rewriting on, the rewriting's own `complete` included (35); EXPLAIN and
/// the plan say what the query is told (36). Under `owl2-dl` with exact answers, the read
/// is refused (422) and its refusal says `not-certified` (N4), though the bounds alone
/// decide it complete (checked first, with the certificate in place).
#[test]
fn a_store_without_a_certificate_is_told_not_certified_on_every_path() {
    let mut cells = Cells::default();
    for (name, ruleset, ql) in [
        ("owl2-rl, ql on", Ruleset::Owl2Rl, QlRewritingMode::On),
        ("owl2-ql", Ruleset::Owl2Ql, QlRewritingMode::Auto),
    ] {
        let store = certified(
            StoreConfig {
                ql_rewriting: ql,
                ..StoreConfig::in_memory()
            },
            ruleset,
        );
        let before = status(&store, READ);
        if matches!(class(&before), "none" | "not-certified") {
            cells.unmet(name, &format!("the store isn't certified: {before}"));
            continue;
        }
        store.invalidate_reasoning().expect("invalidated");
        cells.class(
            &format!("35 the query | {name}"),
            &status(&store, READ),
            "not-certified",
        );
        let [explained, planned] = explained_and_planned(&store, READ);
        cells.class(&format!("36 EXPLAIN | {name}"), &explained, "not-certified");
        cells.class(&format!("36 the plan | {name}"), &planned, "not-certified");
    }
    // A string constant the bounds decide complete (closure_status_tests'
    // `a_string_constant_leaves_an_owl2_dl_answer_complete`).
    let dl = pipeline();
    insert(&dl, ":name a owl:DatatypeProperty . :a0 :name \"Rex\" .").expect("data");
    let store = dl.store();
    let exact = || {
        let mut request =
            SparqlQueryRequest::all(format!("{PREFIXES}SELECT ?v {{ ?v :name \"Rex\" }}"));
        request.dl_answers = Some(nrese_store::DlAnswers::Exact);
        store.execute_query(&request)
    };
    match exact() {
        Ok(answer) if answer.completeness.as_ref().is_some_and(|c| c.complete) => {
            // The regime is a label, not an order: the DL route's result is never told
            // as OWL 2 RL's, whatever ruleset the closure under it was made by (b).
            cells.header(
                "(b) the DL route's regime",
                &header(answer.completeness),
                |h| h.contains("regime=owl2-dl"),
                "regime=owl2-dl",
            );
            store.invalidate_reasoning().expect("invalidated");
            match exact() {
                Err(nrese_store::StoreError::Incomplete(refused)) => cells.class(
                    "N4 exact, no certificate",
                    &refused.status.header(),
                    "not-certified",
                ),
                other => cells.class(
                    "N4 exact, no certificate",
                    &format!("answered: {:?}", other.map(|a| a.completeness)),
                    "a refusal saying not-certified",
                ),
            }
        }
        other => cells.unmet(
            "N4 exact, no certificate",
            &format!(
                "the bounds don't decide it complete: {:?}",
                other.map(|a| a.completeness)
            ),
        ),
    }
    cells.finish();
}

/// What a publication of new statements carries: a rematerialisation's version is read
/// under the program it was built by, the new one, never the one it replaced (the built
/// image's certificate, not the current version's, where its checkpoint is mapped: on disk
/// by default); a load that replaces the data, and one that adds to it while keeping the
/// inferred statements, carry none: their statements are no closure (`not-certified`).
#[test]
fn a_publication_of_new_statements_is_certified_only_by_what_built_them() {
    let mut cells = Cells::default();
    let on_disk = |dir: &Path| certified(StoreConfig::on_disk(dir.join("store")), Ruleset::Owl2Rl);
    let owl2_rl = |cells: &mut Cells, name: &str, store: &StoreService| {
        let before = status(store, READ);
        let certified = before.starts_with("sound-only; regime=owl2-rl");
        if !certified {
            cells.unmet(
                name,
                &format!("the store isn't certified under owl2-rl: {before}"),
            );
        }
        certified
    };
    let dir = tempfile::tempdir().expect("dir");
    let store = on_disk(dir.path());
    if owl2_rl(&mut cells, "a rematerialisation, mapped", &store) {
        store.rematerialise(Ruleset::Rdfs).expect("reasoned again");
        cells.header(
            "a rematerialisation, mapped",
            &status(&store, READ),
            |h| h.starts_with("sound-only; regime=rdfs"),
            "sound-only; regime=rdfs",
        );
    }
    for (name, replace) in [
        ("a load replacing the data", true),
        ("a load adding to it", false),
    ] {
        let dir = tempfile::tempdir().expect("dir");
        let store = served(StoreConfig::on_disk(dir.path().join("store")));
        if owl2_rl(&mut cells, name, &store) {
            load(
                &store,
                ":tom a :Dog . :Cat rdfs:subClassOf :Animal .",
                replace,
            );
            cells.class(name, &status(&store, READ), "not-certified");
        }
    }
    cells.finish();
}

/// A reader who sees the inferences its graphs support reads a view built with the support
/// program the store was told to use (`use_reasoning_rules`). Where that program isn't the
/// one the certificate was made with, the view's inferred statements are no certified
/// closure, and the view may show what a graph the reader can't read asserts: the read is
/// refused until the store is rematerialised (R4), with no program and with another one.
/// Under the matching program the status is the certified one, as `ql_rewriting_tests`'
/// `rewritten_answers_follow_the_readers_graph_access` pins. A server reaches the mismatch
/// while a repository's reasoning changes: the catalog's `settle` publishes the new
/// certificate before the new write path sets the new program.
#[test]
fn a_supported_reader_whose_view_uses_another_program_is_refused() {
    let mut cells = Cells::default();
    let store = certified(
        StoreConfig {
            ql_rewriting: QlRewritingMode::On,
            ..StoreConfig::in_memory()
        },
        Ruleset::Owl2Rl,
    );
    let supported = ReadScope::Graphs(Arc::new(GraphAccess {
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }));
    // The status header, or `refused: …`.
    let read = || {
        let request = SparqlQueryRequest::new(format!("{PREFIXES}{READ}"), supported.clone());
        match store.execute_query(&request) {
            Ok(result) => header(result.completeness),
            Err(error) => format!("refused: {error}"),
        }
    };
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    let matching = read();
    if matches!(class(&matching), "none" | "not-certified") || matching.starts_with("refused") {
        cells.unmet(
            "R4",
            &format!("not certified under the matching program: {matching}"),
        );
    } else {
        for (name, program) in [("no program", None), ("rdfs", Some(Ruleset::Rdfs.into()))] {
            store.use_reasoning_rules(program);
            cells.header(
                &format!("R4 a supported reader's view, {name}"),
                &read(),
                |got| got.starts_with("refused") && got.contains("rematerialis"),
                "a refusal naming rematerialisation",
            );
        }
    }
    cells.finish();
}

/// A reader of `open` who sees inferences, by support or every one, reads the same whether
/// or not a graph it can't read asserts a statement, also while the program the store
/// maintains its stack with isn't its certificate's: a stronger live program finds that
/// `ex:b ex:q ex:a` follows from `open`, which the certified rdfs closure doesn't, and must
/// not show the reader the copy only `secret` asserts: the read is refused until the store is
/// rematerialised, with `secret` or without it. A server reaches the mismatch while a
/// repository's reasoning changes: the catalog's `settle` publishes the new certificate
/// before the new write path sets the new program.
#[test]
fn a_reader_of_some_graphs_learns_nothing_from_a_hidden_graph_under_another_program() {
    const EX: &str = "http://example.com/";
    const OWL: &str = "PREFIX owl: <http://www.w3.org/2002/07/owl#>";
    const OPEN: &str = "ex:p owl:inverseOf ex:q . ex:a ex:p ex:b";
    const SECRET: &str = "ex:b ex:q ex:a";
    let mut cells = Cells::default();
    let reader = |by_support: bool| {
        ReadScope::Graphs(Arc::new(GraphAccess {
            graphs: vec![format!("{EX}open")],
            default_graph: true,
            inferred: true,
            inferred_by_support: by_support,
            ..GraphAccess::default()
        }))
    };
    // The ASK's answer to `scope` (`true`, `false`, or `refused` where the refusal names
    // rematerialisation), `secret` asserting SECRET or not, the stack certified under rdfs and
    // maintained under `live`.
    let asked = |scope: &ReadScope, live: Ruleset, secret: bool| {
        let store = StoreService::new(StoreConfig::in_memory()).expect("store");
        let hidden = if secret {
            format!("GRAPH ex:secret {{ {SECRET} }}")
        } else {
            String::new()
        };
        store
            .execute_update(&SparqlUpdateRequest::new(format!(
                "PREFIX ex: <{EX}> {OWL} INSERT DATA {{ GRAPH ex:open {{ {OPEN} }} {hidden} }}"
            )))
            .expect("data");
        store.rematerialise(Ruleset::Rdfs).expect("reasoned");
        store.use_reasoning_rules(Some(live.into()));
        let request = SparqlQueryRequest::new(
            format!("PREFIX ex: <{EX}> ASK {{ {SECRET} }}"),
            scope.clone(),
        );
        match store.execute_query(&request) {
            Ok(result) => {
                let found = String::from_utf8(result.payload)
                    .expect("utf-8")
                    .contains("true");
                found.to_string()
            }
            Err(error) if error.to_string().contains("rematerialis") => "refused".to_owned(),
            Err(error) => format!("refused otherwise: {error}"),
        }
    };
    for (kind, by_support) in [("visible", false), ("supported", true)] {
        let scope = reader(by_support);
        for (live_name, live, want) in [
            ("rdfs", Ruleset::Rdfs, "false / false"),
            ("owl2-rl", Ruleset::Owl2Rl, "refused / refused"),
        ] {
            let (without, with) = (asked(&scope, live, false), asked(&scope, live, true));
            cells.header(
                &format!("{kind}, {live_name}: the ASK without secret / with it"),
                &format!("{without} / {with}"),
                |got| got == want,
                want,
            );
        }
    }
    cells.finish();
}

/// A commit made without reasoning leaves the inferred statements uncertified; a restart
/// before the next rematerialisation opens them so: `not-certified`, not certified by the
/// open and not read as if no reasoning could leave answers out.
#[test]
fn a_restart_after_a_commit_without_reasoning_opens_not_certified() {
    let mut cells = Cells::default();
    let dir = tempfile::tempdir().expect("dir");
    let config = || StoreConfig::on_disk(dir.path().join("store"));
    {
        let store = Arc::new(StoreService::new(config()).expect("store"));
        let with = |mode| {
            MutationPipeline::new(
                Arc::clone(&store),
                Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
            )
        };
        insert(&with(ReasoningMode::Owl2Rl), DATA).expect("data");
        let before = status(&store, READ);
        if !before.starts_with("sound-only; regime=owl2-rl") {
            cells.unmet("a restart", &format!("the store isn't certified: {before}"));
        }
        insert(&with(ReasoningMode::Disabled), ":tom a :Dog .").expect("commit");
    }
    let store = StoreService::new(config()).expect("reopened");
    cells.class("a restart", &status(&store, READ), "not-certified");
    cells.finish();
}
