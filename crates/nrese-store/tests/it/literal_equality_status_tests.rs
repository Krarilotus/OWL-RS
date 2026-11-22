//! One value in two spellings on a functional property (OWL 2 Profiles §4.3, Table 5
//! prp-fp; Table 6 cls-maxc2 for an at-most-one restriction): the rules equate the two
//! literals, and that `owl:sameAs` has a literal subject, which no store holds. A value
//! written in a later commit then gets no copy in the other spelling (Table 4 eq-rep-o),
//! where the same data in one commit do; under compact equality not even in one commit.
//! While the rules have derived such an equality, a read of a property that holds literals
//! is not told `complete` (docs/limits.md, "two spellings across commits"). The
//! answers stay as they are: only the word drops.

use std::{path::Path, sync::Arc};

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, QlRewritingMode, Requester,
    SolutionsResultFormat, SparqlQueryRequest, SparqlUpdateRequest, StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

const INTEGER: &str = "\"1\"^^xsd:integer";
const DECIMAL: &str = "\"1.0\"^^xsd:decimal";

/// The sources of the equality: a functional property, and an at-most-one restriction.
const SOURCES: [&str; 2] = [
    ":p a owl:FunctionalProperty .",
    ":s a :C . :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; \
     owl:maxCardinality \"1\"^^xsd:nonNegativeInteger ] .",
];

/// The reads of the later value: its values, and their number.
const READS: [&str; 2] = [
    "SELECT ?v WHERE { :t :q ?v }",
    "SELECT (COUNT(*) AS ?n) WHERE { :t :q ?v }",
];

/// The words the withheld status's reason holds.
const REASON: &str = "an owl:sameAs between literals";

const RULESETS: [&str; 2] = ["owl2-dl", "owl2-rl"];
const MODES: [&str; 3] = ["replication", "representatives", "compact"];

fn config(mode: &str, dir: Option<&Path>) -> StoreConfig {
    let mut config = dir.map_or_else(StoreConfig::in_memory, StoreConfig::on_disk);
    config.equality_by_representatives = mode != "replication";
    config.equality_compact = mode == "compact";
    config
}

fn open(ruleset: &str, mut config: StoreConfig) -> MutationPipeline {
    let mode = match ruleset {
        "owl2-dl" => ReasoningMode::Owl2Dl,
        "owl2-rl" => {
            config.ql_rewriting = QlRewritingMode::On;
            ReasoningMode::Owl2Rl
        }
        other => panic!("no ruleset {other}"),
    };
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn update(pipeline: &MutationPipeline, body: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{body}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap_or_else(|error| panic!("{body}: {error:?}"));
}

fn insert(pipeline: &MutationPipeline, data: &str) {
    update(pipeline, &format!("INSERT DATA {{ {data} }}"));
}

/// `query`'s answer (its JSON) and its status: the word and the reasons' texts.
fn read(store: &StoreService, query: &str) -> (String, String, Vec<String>) {
    read_as(store, query, nrese_store::ReadScope::All)
}

/// [`read`] by a reader of `scope`.
fn read_as(
    store: &StoreService,
    query: &str,
    scope: nrese_store::ReadScope,
) -> (String, String, Vec<String>) {
    let mut request = SparqlQueryRequest::new(format!("{PREFIXES}{query}"), scope);
    request.solutions_format = SolutionsResultFormat::Json;
    let result = store.execute_query(&request).expect("read");
    let status = result.completeness.expect("a reasoned read has a status");
    (
        String::from_utf8(result.payload).expect("utf8"),
        status.as_str().to_owned(),
        status.reasons.iter().map(|r| r.text.clone()).collect(),
    )
}

/// Each read of [`READS`] on `store` that is told `complete`, or not for the literal
/// equality, as `case: read: word reasons`.
fn told_otherwise(store: &StoreService, case: &str, unmet: &mut Vec<String>) {
    for query in READS {
        let (_, word, reasons) = read(store, query);
        if word == "complete" || !reasons.iter().any(|r| r.contains(REASON)) {
            unmet.push(format!("{case}: {query}: {word} {reasons:?}"));
        }
    }
}

/// The value written later and the other spelling in the pair, both ways round.
fn spellings() -> [(&'static str, &'static str); 2] {
    [(INTEGER, DECIMAL), (DECIMAL, INTEGER)]
}

#[test]
fn a_value_read_after_two_spellings_were_equated_is_not_complete() {
    let mut unmet = Vec::new();
    let mut controls = Vec::new();
    for ruleset in RULESETS {
        for mode in MODES {
            for (source, axiom) in SOURCES.iter().enumerate() {
                for (written, other) in spellings() {
                    let pair = format!("{axiom} :s :p {written} , {other} .");
                    let later = format!(":t :q {written} .");
                    let case = format!("{ruleset} {mode} source {source} {written}");
                    let two = open(ruleset, config(mode, None));
                    insert(&two, &pair);
                    insert(&two, &later);
                    told_otherwise(two.store(), &format!("{case} two commits"), &mut unmet);
                    let one = open(ruleset, config(mode, None));
                    insert(&one, &format!("{pair} {later}"));
                    told_otherwise(one.store(), &format!("{case} one commit"), &mut unmet);
                    // The same values with no equality between them: the word stays.
                    let plain = open(ruleset, config(mode, None));
                    insert(&plain, &format!(":s :p {written} , {other} . {later}"));
                    let (_, word, reasons) = read(plain.store(), READS[0]);
                    if word != "complete" {
                        controls.push(format!("{case}: {word} {reasons:?}"));
                    }
                }
            }
        }
    }
    assert!(
        unmet.is_empty(),
        "a read after a literal equality is told complete, or not for it:\n{}",
        unmet.join("\n")
    );
    assert!(
        controls.is_empty(),
        "with no equality between the spellings the read is complete:\n{}",
        controls.join("\n")
    );
}

/// Strings have spellings too: a plain string is its `xsd:token` spelling where it has no
/// spaces to collapse (OWL 2 Structural Specification §4.3: `xsd:token` is derived from
/// `xsd:string`), and `"Rex@en"^^rdf:PlainLiteral` is the language-tagged `"Rex"@en`
/// (rdf:PlainLiteral §3). Each pair, then the later string as written.
const STRINGS: [(&str, &str); 2] = [
    ("\"Rex\"", "\"Rex\"^^xsd:token"),
    ("\"Rex\"@en", "\"Rex@en\"^^rdf:PlainLiteral"),
];

/// A plain or a language-tagged string read after its other spelling was equated: the
/// property read holds strings only, and the later string lacks its copy all the same.
#[test]
fn a_string_read_after_another_spelling_was_equated_is_not_complete() {
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        for mode in MODES {
            for (written, other) in STRINGS {
                let pipeline = open(ruleset, config(mode, None));
                insert(
                    &pipeline,
                    &format!("{} :s :p {written} , {other} .", SOURCES[0]),
                );
                insert(&pipeline, &format!(":t :q {written} ."));
                let case = format!("{ruleset} {mode} {written}");
                told_otherwise(pipeline.store(), &case, &mut unmet);
            }
        }
    }
    assert!(
        unmet.is_empty(),
        "a string read after another spelling was equated is told complete:\n{}",
        unmet.join("\n")
    );
}

/// An equality met in a commit on a store that already holds data (the commit's
/// maintenance, not a first closure): the later value is withheld as well.
#[test]
fn an_equality_met_on_a_store_holding_data_is_withheld() {
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        for mode in MODES {
            for (source, axiom) in SOURCES.iter().enumerate() {
                let pipeline = open(ruleset, config(mode, None));
                insert(&pipeline, ":u :r :w .");
                insert(&pipeline, &format!("{axiom} :s :p {INTEGER} , {DECIMAL} ."));
                insert(&pipeline, &format!(":t :q {INTEGER} ."));
                let case = format!("{ruleset} {mode} source {source}");
                told_otherwise(pipeline.store(), &case, &mut unmet);
            }
        }
    }
    assert!(
        unmet.is_empty(),
        "an equality met on a store holding data is told complete:\n{}",
        unmet.join("\n")
    );
}

/// `owl:FunctionalProperty` named through an equality (Table 4 eq-rep-o): `:p` is functional
/// only through `:F`.
const ALIASED: &str = ":F owl:sameAs owl:FunctionalProperty . :p a :F .";

/// A library store reopened from disk without a rebuild: its marker doesn't record whether the
/// rules equated two literals, so its reads of literals are withheld, whatever its data hold,
/// until a rebuild (a rematerialisation) finds no such equality. The pair on a functional
/// property; that property made functional through an equality ([`ALIASED`], `owl2-rl`); the
/// pair written under compact equality and reopened with it off (rebuilt at the first write);
/// and two spellings with nothing to equate them. Rebuilt, only the last is told `complete`.
#[test]
fn a_store_reopened_without_a_rebuild_withholds_the_word_until_one_finds_no_literal_equality() {
    let words = |store: &StoreService| READS.map(|query| read(store, query).1);
    let pair = format!(":s :p {INTEGER} , {DECIMAL} .");
    // Name, axiom, the mode it is reopened in (else its own), whether the rules equate the pair.
    let cases = [
        ("functional", SOURCES[0], None, true),
        ("aliased", ALIASED, None, true),
        (
            "compact reopened expanded",
            SOURCES[0],
            Some("representatives"),
            true,
        ),
        ("no equality", "", None, false),
    ];
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        for mode in MODES {
            for (name, axiom, reopened, equated) in cases {
                if (name == "aliased" && ruleset != "owl2-rl")
                    || (reopened.is_some() && mode != "compact")
                {
                    continue;
                }
                let case = format!("{ruleset} {mode} {name}");
                let dir = tempfile::tempdir().expect("dir");
                {
                    let before = open(ruleset, config(mode, Some(dir.path())));
                    insert(&before, &format!("{axiom} {pair}"));
                }
                let after = open(ruleset, config(reopened.unwrap_or(mode), Some(dir.path())));
                insert(&after, &format!(":t :q {INTEGER} ."));
                let store = after.store();
                // A compact stack opened with compact equality off is another form of the
                // stack: its first write rebuilds it, and that rebuild says.
                assert!(
                    reopened.is_some() || store.last_materialisation().is_none(),
                    "{case}: the reopened store was rematerialised"
                );
                told_otherwise(store, &format!("{case} reopened"), &mut unmet);
                let rules = after.reasoner().config().materialised_program();
                store.rematerialise(rules.expect("rules")).expect("rebuild");
                if equated {
                    told_otherwise(store, &format!("{case} rebuilt"), &mut unmet);
                } else if words(store) != ["complete", "complete"] {
                    unmet.push(format!("{case} rebuilt: {:?}", words(store)));
                }
            }
        }
    }
    assert!(
        unmet.is_empty(),
        "a store reopened without a rebuild is told otherwise:\n{}",
        unmet.join("\n")
    );
}

/// The words the reason holds where the store was opened without a rebuild.
const REOPENED: &str = "opened without a rebuild";

/// A reader by support of a store reopened without a rebuild: whether the rules equated two
/// literals since the last rebuild is the whole store's unknown, which no view can settle, so
/// its reads of literals are withheld as every reader's are, with the reason that the store
/// was opened without a rebuild. The value `"Rex"` in `:open`, which nothing could equate,
/// read by a reader by support of `:open` (and the default graph), under `owl2-ql` and under
/// `owl2-rl` with the rewriting: `complete` before the reopen, withheld after it for every
/// reader, and `complete` again after a rebuild.
#[test]
fn a_reader_by_support_of_a_store_reopened_without_a_rebuild_is_withheld_too() {
    use nrese_reasoner::rulesets::Ruleset;
    use nrese_store::ReadScope;

    use crate::support::Writes;
    let data = format!(
        "{PREFIXES}INSERT DATA {{ :p a owl:DatatypeProperty . GRAPH :open {{ :a :p \"Rex\" }} }}"
    );
    let query = "SELECT ?x ?n { GRAPH :open { ?x :p ?n } }";
    let mut unmet = Vec::new();
    for (regime, ruleset, ql) in [
        ("owl2-ql", Ruleset::Owl2Ql, QlRewritingMode::Auto),
        ("owl2-rl rewriting on", Ruleset::Owl2Rl, QlRewritingMode::On),
    ] {
        let dir = tempfile::tempdir().expect("dir");
        let config = || StoreConfig {
            ql_rewriting: ql,
            ..StoreConfig::on_disk(dir.path())
        };
        {
            let store = StoreService::new(config()).expect("store");
            (store.execute_update(&SparqlUpdateRequest::new(data.clone()))).expect("data");
            store.rematerialise(ruleset).expect("rebuild");
            store.use_reasoning_rules(Some(ruleset.into()));
            let (_, word, reasons) = read_as(&store, query, reader_of_open(true));
            if word != "complete" {
                unmet.push(format!(
                    "setup: {regime} before the reopen: {word} {reasons:?}"
                ));
            }
        }
        let store = StoreService::new(config()).expect("reopen");
        store.use_reasoning_rules(Some(ruleset.into()));
        assert!(
            store.last_materialisation().is_none(),
            "{regime}: the reopened store was rematerialised"
        );
        for (reader, scope) in [
            ("by support", reader_of_open(true)),
            ("of every graph", ReadScope::All),
        ] {
            let (_, word, reasons) = read_as(&store, query, scope);
            if word == "complete" || !reasons.iter().any(|r| r.contains(REOPENED)) {
                unmet.push(format!("{regime} {reader} reopened: {word} {reasons:?}"));
            }
        }
        store.rematerialise(ruleset).expect("rebuild");
        let (_, word, reasons) = read_as(&store, query, reader_of_open(true));
        if word != "complete" {
            unmet.push(format!("{regime} by support rebuilt: {word} {reasons:?}"));
        }
    }
    assert!(
        unmet.is_empty(),
        "a reader of a store reopened without a rebuild is told otherwise:\n{}",
        unmet.join("\n")
    );
}

/// The condition stays while the store holds the pair's derivation, a delete of its premise
/// included (the commit can't tell which later value lacks a copy); a rebuild that finds no
/// literal equality clears it, and one that finds it again keeps it.
#[test]
fn only_a_rebuild_that_finds_no_literal_equality_gives_the_word_back() {
    for mode in MODES {
        let pipeline = open("owl2-rl", config(mode, None));
        let store = pipeline.store();
        insert(
            &pipeline,
            &format!("{} :s :p {INTEGER} , {DECIMAL} .", SOURCES[0]),
        );
        insert(&pipeline, &format!(":t :q {INTEGER} ."));
        let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
            .materialised_program()
            .expect("owl2-rl rules");
        store.rematerialise(rules.clone()).expect("rebuild");
        let mut unmet = Vec::new();
        told_otherwise(store, &format!("{mode} rebuilt with the pair"), &mut unmet);
        update(&pipeline, &format!("DELETE DATA {{ :s :p {DECIMAL} }}"));
        told_otherwise(store, &format!("{mode} the pair deleted"), &mut unmet);
        assert!(
            unmet.is_empty(),
            "withheld until a clean rebuild:\n{}",
            unmet.join("\n")
        );
        store.rematerialise(rules).expect("rebuild");
        let (_, word, reasons) = read(store, READS[0]);
        assert_eq!(
            word, "complete",
            "{mode}: a rebuild with no literal equality gives the word back: {reasons:?}"
        );
    }
}

/// A reader of some graphs is told nothing of a graph it can't read (docs/spec/
/// reasoning-semantics.md, "What may be `complete`": a restricted reader's status says
/// nothing about graphs they can't read). With the two spellings equated in such a graph,
/// and the same values there with nothing to equate them, its reads of the same data it
/// reads are told alike, word and reasons: whether it sees every inference or those its
/// graphs support, under `owl2-rl` with the rewriting and without, and under `owl2-dl`. With
/// the pair equated in a graph it reads, a reader by support is told it.
#[test]
fn a_reader_of_some_graphs_is_told_alike_with_a_hidden_literal_equality_and_without() {
    use nrese_store::ReadScope;
    let visible = format!(":t :q {INTEGER} . :t a :A . GRAPH :open {{ :u :q {DECIMAL} . }}");
    let reads = [
        "SELECT ?v WHERE { :t :q ?v }",
        "SELECT ?x WHERE { ?x a :A }",
    ];
    let reader = reader_of_open;
    let mut unmet = Vec::new();
    for (ruleset, rewriting) in [
        ("owl2-rl", None),
        ("owl2-rl", Some(QlRewritingMode::Off)),
        ("owl2-dl", None),
    ] {
        let store = |graph: &str, axiom: &str| {
            let pipeline = open(ruleset, config("replication", None));
            if let Some(mode) = rewriting {
                pipeline.store().set_ql_rewriting(mode);
            }
            let pair = format!("GRAPH {graph} {{ {axiom} :s :p {INTEGER} , {DECIMAL} . }}");
            insert(&pipeline, &format!("{visible} {pair}"));
            pipeline
        };
        let case = format!("{ruleset} {rewriting:?}");
        let (equated, plain) = (store(":hidden", SOURCES[0]), store(":hidden", ""));
        let every = read_as(equated.store(), reads[0], ReadScope::All).2;
        if !every.iter().any(|r| r.contains(REASON)) {
            unmet.push(format!(
                "setup: {case}: a reader of every graph isn't told: {every:?}"
            ));
        }
        // The control: the pair equated in a graph it reads, a reader by support is told,
        // under the rules (`owl2-dl` tells every reader of some graphs `not-certified`).
        if ruleset == "owl2-rl" {
            let own = store(":open", SOURCES[0]);
            let (_, word, reasons) = read_as(own.store(), reads[0], reader(true));
            if word == "complete" || !reasons.iter().any(|r| r.contains(REASON)) {
                unmet.push(format!("{case}: in a graph it reads: {word} {reasons:?}"));
            }
        }
        for by_support in [false, true] {
            for query in reads {
                let told = read_as(equated.store(), query, reader(by_support));
                let alone = read_as(plain.store(), query, reader(by_support));
                if (&told.1, &told.2) != (&alone.1, &alone.2) {
                    unmet.push(format!(
                        "{case} by support {by_support}: {query}: {} {:?}, not {} {:?}",
                        told.1, told.2, alone.1, alone.2
                    ));
                }
            }
        }
    }
    assert!(
        unmet.is_empty(),
        "a reader of some graphs is told of an equality in a graph it can't read:\n{}",
        unmet.join("\n")
    );
}

/// A reader of the default graph and `:open`, not `:hidden`, who sees every inference
/// (`inferred = "visible"`) or, `by_support`, those its graphs support.
fn reader_of_open(by_support: bool) -> nrese_store::ReadScope {
    nrese_store::ReadScope::Graphs(Arc::new(nrese_sparql::GraphAccess {
        graphs: vec!["http://example.com/open".to_owned()],
        default_graph: true,
        inferred: true,
        inferred_by_support: by_support,
        ..nrese_sparql::GraphAccess::default()
    }))
}

/// A property made functional by what the rules derive from a graph the reader by support
/// reads (`:F rdfs:subClassOf owl:FunctionalProperty`, Table 7 cax-sco), with the two
/// spellings there: its view holds the functional property as an inference, and the rules
/// equate the two literals, so a read of a later value is told so, as where that graph
/// declares the property functional.
#[test]
fn a_reader_by_support_is_told_of_an_equality_its_graphs_derive() {
    let data = format!(
        ":t :q {INTEGER} . GRAPH :open {{ :F rdfs:subClassOf owl:FunctionalProperty . \
         :p a :F . :s :p {INTEGER} , {DECIMAL} . }}"
    );
    let mut unmet = Vec::new();
    for rewriting in [None, Some(QlRewritingMode::Off)] {
        let pipeline = open("owl2-rl", config("replication", None));
        if let Some(mode) = rewriting {
            pipeline.store().set_ql_rewriting(mode);
        }
        insert(&pipeline, &data);
        let every = read(pipeline.store(), READS[0]).2;
        if !every.iter().any(|r| r.contains(REASON)) {
            unmet.push(format!(
                "setup: {rewriting:?}: a reader of every graph isn't told: {every:?}"
            ));
        }
        let (_, word, reasons) = read_as(pipeline.store(), READS[0], reader_of_open(true));
        if word == "complete" || !reasons.iter().any(|r| r.contains(REASON)) {
            unmet.push(format!("{rewriting:?}: {word} {reasons:?}"));
        }
    }
    assert!(
        unmet.is_empty(),
        "a reader by support isn't told of an equality its own graphs derive:\n{}",
        unmet.join("\n")
    );
}

/// Certification suspended by rules of another ruleset (`use_reasoning_rules`, as an embedding
/// caller may) while the store keeps its `owl2-rl` record: the `owl2-rl` pipeline still
/// maintains the closure, and a commit that equates two literals then has no certificate to
/// mark. Certified again under `owl2-rl`, a read of a later value is still withheld.
#[test]
fn an_equality_committed_while_certification_is_suspended_stays_withheld() {
    use nrese_reasoner::rulesets::Ruleset;
    let mut unmet = Vec::new();
    for mode in MODES {
        let pipeline = open("owl2-rl", config(mode, None));
        let store = pipeline.store();
        insert(&pipeline, ":u :r :w .");
        store.use_reasoning_rules(Some(Ruleset::Rdfs.into()));
        insert(
            &pipeline,
            &format!("{} :s :p {INTEGER} , {DECIMAL} .", SOURCES[0]),
        );
        store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
        insert(&pipeline, &format!(":t :q {INTEGER} ."));
        told_otherwise(store, mode, &mut unmet);
    }
    assert!(
        unmet.is_empty(),
        "an equality committed while certification was suspended is told complete after:\n{}",
        unmet.join("\n")
    );
}
