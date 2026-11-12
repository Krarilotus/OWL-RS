//! Q07 (N12, RSH-01/03, SYN-65): a rejection shown to a requester who may read only some
//! graphs depends only on statements of those graphs. That the commit is rejected is the
//! approved global-gate oracle (D-1); its terms, counts, engine, time and focus are not.
//! A requester who may read every graph keeps the full evidence (the `*_full_*` controls,
//! which pass before the fix too).

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::GraphAccess;
use nrese_store::{
    GateSeverity, MutationCommand, MutationError, MutationPipeline, MutationReject, MutationTicket,
    ReadScope, RejectKind, Requester, ShaclGate, SparqlUpdateRequest, StoreConfig, StoreService,
    WriteScope,
};

const P: &str = "PREFIX : <http://example.com/> PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX sh: <http://www.w3.org/ns/shacl#> ";

fn update(text: &str) -> MutationCommand {
    MutationCommand::Update(SparqlUpdateRequest::new(format!(
        "{P}INSERT DATA {{ {text} }}"
    )))
}

/// May read and write `:open` only.
fn open_reader() -> Requester {
    let access = Arc::new(GraphAccess {
        graphs: vec!["http://example.com/open".into()],
        ..GraphAccess::default()
    });
    Requester::new(
        ReadScope::Graphs(Arc::clone(&access)),
        WriteScope::Graphs(access),
    )
}

/// The rejection `requester` gets for `commit` on a fresh store holding `data`.
fn rejection(
    config: StoreConfig,
    mode: ReasoningMode,
    data: &str,
    commit: &str,
    requester: &Requester,
) -> MutationReject {
    let p = MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    p.apply(update(data), &Requester::all(), &MutationTicket::new())
        .expect("setup commits");
    match p.apply(update(commit), requester, &MutationTicket::new()) {
        Err(MutationError::Rejected(reject)) => *reject,
        other => panic!("must reject: {other:?}"),
    }
}

/// A functional data property and its `"SECRET"` value, both in `:secret`; the commit
/// gives the subject a second value.
const RL_DATA: &str =
    "GRAPH :secret { :p a owl:FunctionalProperty, owl:DatatypeProperty . :a :p \"SECRET\" }";
const RL_COMMIT: &str = "GRAPH :open { :a :p \"new\" }";

fn rl(requester: &Requester) -> MutationReject {
    let config = StoreConfig::in_memory();
    rejection(config, ReasoningMode::Owl2Rl, RL_DATA, RL_COMMIT, requester)
}

/// RSH-01: an OWL 2 RL rejection never decodes a term of a premise in a graph the
/// committer can't read.
#[test]
fn rl_rejection_does_not_decode_private_bindings() {
    let reject = rl(&open_reader());
    assert!(!format!("{reject:?}").contains("SECRET"), "{reject:?}");
}

/// Control: a reader of every graph sees the clashing values.
#[test]
fn rl_rejection_full_reader_sees_the_values() {
    let reject = rl(&Requester::all());
    assert!(reject.detail.contains("SECRET"), "{}", reject.detail);
}

/// An open subclass axiom whose superclass, a restriction on `:SECRET_PROPERTY`, is
/// described in `:secret`.
const DL_STRUCTURAL_DATA: &str = "GRAPH :open { :A rdfs:subClassOf _:r } \
     GRAPH :secret { _:r a owl:Restriction ; owl:onProperty :SECRET_PROPERTY ; \
     owl:someValuesFrom owl:Nothing }";
const DL_STRUCTURAL_COMMIT: &str = "GRAPH :open { :a a :A }";

fn dl_structural(requester: &Requester) -> MutationReject {
    let (config, mode) = (StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    rejection(
        config,
        mode,
        DL_STRUCTURAL_DATA,
        DL_STRUCTURAL_COMMIT,
        requester,
    )
}

/// RSH-03: each structural triple of an OWL 2 DL axiom is shown by the graph it is in,
/// not the graph of the axiom's first triple.
#[test]
fn dl_structural_triples_keep_their_own_graph() {
    let reject = dl_structural(&open_reader());
    let shown = format!("{reject:?}");
    assert!(!shown.contains("SECRET_PROPERTY"), "{shown}");
}

/// Control: a reader of every graph sees the restriction's triples.
#[test]
fn dl_structural_full_reader_sees_the_restriction() {
    let reject = dl_structural(&Requester::all());
    let evidence = reject.explanation.expect("an explanation").evidence;
    let shown = |e: &&nrese_reasoner::RejectEvidence| e.object.contains("SECRET_PROPERTY");
    assert!(evidence.iter().any(|e| shown(&e)), "{evidence:#?}");
}

/// A justification of five axioms, three of them in `:secret`; the commit's two type
/// statements are the others.
const DL_COUNT_DATA: &str = "GRAPH :secret { :A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
     :B owl:disjointWith :D . :C owl:disjointWith :D }";
const DL_COUNT_COMMIT: &str = "GRAPH :open { :x a :A, :D }";

fn dl_count(requester: &Requester) -> MutationReject {
    let (config, mode) = (StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
    rejection(config, mode, DL_COUNT_DATA, DL_COUNT_COMMIT, requester)
}

/// SYN-65: the OWL 2 DL rejection text has no number or name that depends on the secret
/// axioms: no withheld-statement count, no justification size, no engine, no time.
#[test]
fn dl_rejection_has_no_hidden_proof_counts() {
    let shown = format!("{:?}", dl_count(&open_reader()));
    for leak in [
        "of their statements",
        "axiom(s)",
        "hypertableau",
        "context-core",
        " ms)",
    ] {
        assert!(!shown.contains(leak), "{leak:?} in {shown}");
    }
}

/// Control: a reader of every graph keeps the justification's size and the check's time.
#[test]
fn dl_rejection_full_reader_keeps_counts_and_time() {
    let detail = dl_count(&Requester::all()).detail;
    assert!(
        detail.contains("5 axiom(s) have no model together"),
        "{detail}"
    );
    assert!(detail.contains(" ms)"), "{detail}");
}

/// A shape requiring `:p` of every `:B`; `:SECRET_FOCUS a :A` is in `:secret`, and the
/// commit makes every `:A` a `:B`.
fn shacl(requester: &Requester) -> MutationReject {
    let config = StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::in_memory()
    };
    let data = format!(
        "GRAPH <{}> {{ :S a sh:NodeShape ; sh:targetClass :B ; \
         sh:property [ sh:path :p ; sh:minCount 1 ] }} GRAPH :secret {{ :SECRET_FOCUS a :A }}",
        config.shapes_graph
    );
    let commit = "GRAPH :open { :A rdfs:subClassOf :B }";
    rejection(config, ReasoningMode::Owl2Rl, &data, commit, requester)
}

/// N12: a SHACL gate rejection doesn't name a focus node that only a graph the committer
/// can't read mentions, nor count results that depend on such graphs.
#[test]
fn shacl_rejection_does_not_name_private_focus() {
    let shown = format!("{:?}", shacl(&open_reader()));
    assert!(!shown.contains("SECRET_FOCUS"), "{shown}");
    assert!(!shown.contains("1 SHACL result(s)"), "{shown}");
}

/// Control: a reader of every graph sees the focus node and the count.
#[test]
fn shacl_rejection_full_reader_sees_the_focus() {
    let detail = shacl(&Requester::all()).detail;
    assert!(detail.contains("SECRET_FOCUS"), "{detail}");
    assert!(detail.contains("1 SHACL result(s)"), "{detail}");
}

// The capability kept: what the committer's own graphs show by themselves is explained to
// it in full.

/// OWL 2 RL: the disjointness and the earlier type are in `:open`. Until Q07 S1b (a
/// bounded, charged ground program of the view) the OWL 2 RL reason of a view isn't looked
/// for: the restricted committer is told only the common sentence, never part of the whole
/// data's violation. S1b turns this into the violation, its rule and its premises.
#[test]
fn rl_violation_of_the_readable_graphs_is_not_yet_explained() {
    let data = "GRAPH :open { :A owl:disjointWith :B . :x a :A }";
    let commit = "GRAPH :open { :x a :B }";
    let config = StoreConfig::in_memory();
    let reject = rejection(config, ReasoningMode::Owl2Rl, data, commit, &open_reader());
    assert!(reject.explanation.is_none(), "{reject:?}");
    assert!(reject.attribution.is_none(), "{reject:?}");
    assert!(
        reject.detail.contains("show no reason by themselves"),
        "{}",
        reject.detail
    );
}

/// OWL 2 DL: the SYN-65 axioms in `:open` itself; the restricted committer is told the
/// justification of its own graphs.
#[test]
fn dl_inconsistency_of_the_readable_graphs_is_explained() {
    let data = "GRAPH :open { :A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
         :B owl:disjointWith :D . :C owl:disjointWith :D }";
    let config = StoreConfig::in_memory();
    let reject = rejection(
        config,
        ReasoningMode::Owl2Dl,
        data,
        DL_COUNT_COMMIT,
        &open_reader(),
    );
    let explanation = reject.explanation.expect("explained");
    assert_eq!(explanation.violated_constraint, "owl2-dl-consistency");
    let shown = |p: &str| explanation.evidence.iter().any(|e| e.predicate.contains(p));
    assert!(shown("unionOf") && shown("disjointWith"), "{explanation:?}");
    assert!(reject.detail.contains("5 axiom(s)"), "{}", reject.detail);
}

/// May read `:open` and the shapes graph.
fn shapes_reader() -> Requester {
    let access = Arc::new(GraphAccess {
        graphs: vec![
            "http://example.com/open".into(),
            StoreConfig::in_memory().shapes_graph,
        ],
        ..GraphAccess::default()
    });
    Requester::new(
        ReadScope::Graphs(Arc::clone(&access)),
        WriteScope::Graphs(access),
    )
}

fn shacl_with(data: &str, commit: &str, requester: &Requester) -> MutationReject {
    let config = StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::in_memory()
    };
    let data = format!(
        "GRAPH <{}> {{ :S a sh:NodeShape ; sh:targetClass :B ; \
         sh:property [ sh:path :p ; sh:minCount 1 ] }} {data}",
        config.shapes_graph
    );
    rejection(config, ReasoningMode::Owl2Rl, &data, commit, requester)
}

/// SHACL: the shape and the focus node are readable; the restricted committer is told the
/// focus node and the component.
#[test]
fn shacl_result_of_the_readable_graphs_is_explained() {
    let reject = shacl_with("", "GRAPH :open { :b a :B }", &shapes_reader());
    assert!(reject.detail.contains("example.com/b"), "{}", reject.detail);
    assert!(reject.detail.contains("MinCount"), "{}", reject.detail);
}

/// The kind of reason a rejection names is the kind of the reason shown, not the gate's: a
/// reader of every graph is told the rejecting gate's kind; a restricted committer the kind
/// of the reason its graphs show, or the same kind whichever gate rejected when they show
/// none (D16-503).
#[test]
fn a_rejection_names_the_kind_of_reason_shown() {
    let all = Requester::all();
    assert_eq!(rl(&all).kind, RejectKind::Inconsistent);
    assert_eq!(dl_count(&all).kind, RejectKind::Inconsistent);
    assert_eq!(shacl(&all).kind, RejectKind::ShapesViolated);
    let view_dl = rejection(
        StoreConfig::in_memory(),
        ReasoningMode::Owl2Dl,
        "GRAPH :open { :A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
         :B owl:disjointWith :D . :C owl:disjointWith :D }",
        DL_COUNT_COMMIT,
        &open_reader(),
    );
    assert_eq!(view_dl.kind, RejectKind::Inconsistent);
    let view_shacl = shacl_with("", "GRAPH :open { :b a :B }", &shapes_reader());
    assert_eq!(view_shacl.kind, RejectKind::ShapesViolated);
    assert_eq!(rl(&open_reader()).kind, RejectKind::NotShown);
    assert_eq!(shacl(&open_reader()).kind, RejectKind::NotShown);
}

// Noninterference (Q07's invariant): two states whose statements in the committer's
// graphs are the same, both rejected, give it the same rejection, whichever gate rejected
// and whatever the other graphs hold.

/// The two rejections are equal, field by field (no text of them is compared or rewritten:
/// that could hide a difference). A fixture whose rejection shows blank nodes gives them
/// fixed labels ([`Trig`]): SPARQL `INSERT DATA` labels each fresh, so two stores would
/// differ by construction.
fn same_for_both(a: MutationReject, b: MutationReject) {
    assert_eq!(a.kind, b.kind);
    assert_eq!(a.detail, b.detail);
    assert_eq!(a.explanation, b.explanation);
    assert_eq!(a.attribution, b.attribution);
}

/// A write of TriG statements as they are written, blank node labels kept: the library
/// write path ([`MutationCommand::Delta`]), gated as every write is.
#[derive(Debug)]
struct Trig(String);

impl nrese_store::DeltaPlanner for Trig {
    fn name(&self) -> &str {
        "trig"
    }

    fn plan(
        &self,
        tx: &mut nrese_engine::Transaction<'_>,
        _: &Requester,
        _: &nrese_store::UpdateContext<'_>,
    ) -> Result<(), nrese_store::StoreError> {
        let text = format!(
            "@prefix : <http://example.com/> . \
             @prefix owl: <http://www.w3.org/2002/07/owl#> . \
             @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> . \
             @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . \
             @prefix sh: <http://www.w3.org/ns/shacl#> . {}",
            self.0
        );
        let parser = nrese_rdf_io::RdfParser::from_format(nrese_rdf_io::RdfFormat::TriG);
        for quad in parser.for_slice(text.as_bytes()) {
            tx.insert(quad.expect("TriG").as_ref());
        }
        Ok(())
    }
}

fn trig(text: &str) -> MutationCommand {
    MutationCommand::Delta(Arc::new(Trig(text.to_owned())))
}

/// OWL 2 RL: a disjointness, or a subclass into a disjointness, in `:secret`.
#[test]
fn rl_rejection_depends_only_on_the_readable_graphs() {
    let open = "GRAPH :open { :x a :A }";
    let commit = "GRAPH :open { :x a :C }";
    let run = |secret: &str| {
        let data = format!("{open} GRAPH :secret {{ {secret} }}");
        let config = StoreConfig::in_memory();
        rejection(config, ReasoningMode::Owl2Rl, &data, commit, &open_reader())
    };
    same_for_both(
        run(":A owl:disjointWith :C"),
        run(":C rdfs:subClassOf :B . :B owl:disjointWith :A . :y :q :z"),
    );
}

/// Across gates: the same commit, rejected by the OWL 2 RL gate (a plain disjointness) or
/// by the OWL 2 DL gate (the SYN-65 union), tells the committer the same.
#[test]
fn dl_rejection_depends_only_on_the_readable_graphs() {
    let run = |secret: &str| {
        let data = format!("GRAPH :secret {{ {secret} }}");
        let config = StoreConfig::in_memory();
        rejection(
            config,
            ReasoningMode::Owl2Dl,
            &data,
            DL_COUNT_COMMIT,
            &open_reader(),
        )
    };
    same_for_both(run(":A owl:disjointWith :D"), run(DL_COUNT_DATA_SECRET));
}

/// The SYN-65 axioms, without their graph.
const DL_COUNT_DATA_SECRET: &str = ":A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
     :B owl:disjointWith :D . :C owl:disjointWith :D";

/// SHACL: a different focus node, and a different number of them, in `:secret`.
#[test]
fn shacl_rejection_depends_only_on_the_readable_graphs() {
    let commit = "GRAPH :open { :A rdfs:subClassOf :B }";
    let run = |secret: &str| shacl_with(secret, commit, &shapes_reader());
    same_for_both(
        run("GRAPH :secret { :s1 a :A }"),
        run("GRAPH :secret { :s2 a :A . :s3 a :A }"),
    );
}

// Sol-loop r1 (S1–S3): the view's reasons don't depend on the gates' own state.

/// The rejection `requester` gets for `commit` on a fresh store after the `setup` writes
/// (by a reader of every graph), in order.
fn rejected_after(
    config: StoreConfig,
    mode: ReasoningMode,
    setup: &[MutationCommand],
    commit: MutationCommand,
    requester: &Requester,
) -> MutationReject {
    let p = MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    for command in setup {
        p.apply(command.clone(), &Requester::all(), &MutationTicket::new())
            .expect("setup commits");
    }
    match p.apply(commit, requester, &MutationTicket::new()) {
        Err(MutationError::Rejected(reject)) => *reject,
        other => panic!("must reject: {other:?}"),
    }
}

fn reject_on(p: &MutationPipeline, commit: &str, requester: &Requester) -> MutationReject {
    match p.apply(update(commit), requester, &MutationTicket::new()) {
        Err(MutationError::Rejected(reject)) => *reject,
        other => panic!("must reject: {other:?}"),
    }
}

/// An OWL 2 RL store holding `data` as loaded without the gate (inconsistent data is
/// tolerated: quarantine).
fn loaded_past_the_gate(data: &str) -> MutationPipeline {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    let reasoner = |mode| Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode)));
    MutationPipeline::new(Arc::clone(&store), reasoner(ReasoningMode::Disabled))
        .apply(update(data), &Requester::all(), &MutationTicket::new())
        .expect("loaded without reasoning");
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
        .expect("rematerialised");
    MutationPipeline::new(store, reasoner(ReasoningMode::Owl2Rl))
}

/// S1: the violation of the committer's own graphs (`:x`) is shown whether or not a secret
/// copy of `:x a :B` made it one of the data's before (then the gate lists only the secret
/// `:y` one, as it lists only what a commit introduces).
#[test]
fn rl_rejection_in_quarantine_depends_only_on_the_readable_graphs() {
    let run = |copy: &str| {
        let p = loaded_past_the_gate(&format!(
            "GRAPH :open {{ :A owl:disjointWith :B . :x a :A }} \
             GRAPH :secret {{ :C owl:disjointWith :D . :y a :C {copy} }}"
        ));
        reject_on(&p, "GRAPH :open { :x a :B . :y a :D }", &open_reader())
    };
    let (without, with) = (run(""), run(". :x a :B"));
    same_for_both(without, with);
}

/// S2: a write to a graph the committer may not read never reaches its attribution: whether
/// it changed anything depends on what that graph held.
#[test]
fn rl_attribution_depends_only_on_the_readable_graphs() {
    let graphs = |names: &[&str]| {
        Arc::new(GraphAccess {
            graphs: names
                .iter()
                .map(|g| format!("http://example.com/{g}"))
                .collect(),
            ..GraphAccess::default()
        })
    };
    let writer = Requester::new(
        ReadScope::Graphs(graphs(&["open"])),
        WriteScope::Graphs(graphs(&["open", "secret"])),
    );
    let run = |secret: &str| {
        let data = format!("GRAPH :open {{ :A owl:disjointWith :B . :x a :A }} {secret}");
        let config = StoreConfig::in_memory();
        let commit = "GRAPH :open { :x a :B } GRAPH :secret { :x :p :q }";
        rejection(config, ReasoningMode::Owl2Rl, &data, commit, &writer)
    };
    let (new, held) = (run(""), run("GRAPH :secret { :x :p :q }"));
    same_for_both(new, held);
}

/// Two consistency rules of one name: the first on a premise `:secret` may hold, the second
/// on the committer's own statement.
const SAME_NAME_RULES: &str = r#"
Prefices
{
  ex : http://example.com/
}

Axioms
{
  <ex:visible> <ex:label> "visible"
}

Rules
{
Consistency: r
  x <ex:hidden> y
  x <ex:trigger> <ex:t>
  ---------------

Consistency: r
  x <ex:visible> y
  ---------------
}
"#;

/// Sol-loop r2-S4: the view's violation of the second rule isn't explained by the first
/// rule of its name, whose premise is a statement of `:secret` (asserted there or not).
#[test]
fn rl_view_violation_isnt_explained_by_another_rule_of_its_name() {
    let run = |secret: &str| {
        let rules = Arc::new(
            nrese_reasoner::UserRules::pie("same-name.pie", SAME_NAME_RULES).expect("compiles"),
        );
        let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
            .with_rules(Some(rules))
            .expect("a mode for the rules");
        let p = MutationPipeline::new(
            Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store")),
            Arc::new(ReasonerService::new(config)),
        );
        if !secret.is_empty() {
            p.apply(update(secret), &Requester::all(), &MutationTicket::new())
                .expect("setup commits");
        }
        reject_on(
            &p,
            "GRAPH :open { :s :visible :v ; :trigger :t }",
            &open_reader(),
        )
    };
    same_for_both(run(""), run("GRAPH :secret { :s :hidden :v }"));
}

/// S3: the view's OWL 2 DL inconsistency is shown whether the DL gate rejected or, with a
/// secret annotation-property declaration making the whole data consistent, the SHACL gate
/// did: the RDF mapping isn't monotone.
#[test]
fn dl_reason_of_the_view_doesnt_depend_on_the_dl_verdict() {
    let config = || StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::in_memory()
    };
    let run = |secret: &str| {
        // Fixed blank node labels: the union and its list are shown.
        let data = format!(
            "GRAPH <{}> {{ :S a sh:NodeShape ; sh:targetClass :T ; sh:property _:ps .              _:ps sh:path :needed ; sh:minCount 1 }}              GRAPH :open {{ :U owl:equivalentClass _:u . _:u owl:unionOf _:l1 .              _:l1 rdf:first :B ; rdf:rest _:l2 . _:l2 rdf:first :C ; rdf:rest rdf:nil .              :B owl:disjointWith :D . :C owl:disjointWith :D . :y a :D . :p rdfs:range :U }}              {secret}",
            config().shapes_graph
        );
        let commit = update("GRAPH :open { :x :p :y . :z a :T }");
        rejected_after(
            config(),
            ReasoningMode::Owl2Dl,
            &[trig(&data)],
            commit,
            &shapes_reader(),
        )
    };
    let (dl, shacl) = (
        run(""),
        run("GRAPH :secret { :p a owl:AnnotationProperty }"),
    );
    assert!(dl.detail.contains("OWL 2 DL"), "{}", dl.detail);
    assert!(format!("{dl:?}").contains("_:u"), "{dl:?}");
    same_for_both(dl, shacl);
}

// Package review r1: what is shown, and in which order, doesn't follow term ids, and a
// refused shapes graph a committer may not read isn't described to it.

/// The rejection `requester` gets for `commit` on a fresh store where `first` was committed
/// before `data`: `first` only decides which terms are interned first.
fn after_first(
    config: StoreConfig,
    mode: ReasoningMode,
    first: &str,
    data: &str,
    commit: &str,
    requester: &Requester,
) -> MutationReject {
    let p = MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    for setup in [first, data] {
        p.apply(update(setup), &Requester::all(), &MutationTicket::new())
            .expect("setup commits");
    }
    reject_on(&p, commit, requester)
}

/// R1-D1 (SHACL): two readable focus nodes break the shape; which one is shown doesn't
/// depend on which of them a secret statement made the dictionary know first.
#[test]
fn shacl_focus_shown_doesnt_follow_term_ids() {
    let run = |first: &str| {
        let config = StoreConfig {
            shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
            ..StoreConfig::in_memory()
        };
        let shapes = format!(
            "GRAPH <{}> {{ :S a sh:NodeShape ; sh:targetClass :T ; \
             sh:property [ sh:path :p ; sh:minCount 1 ] }}",
            config.shapes_graph
        );
        let commit = "GRAPH :open { :q07FocusA a :T . :q07FocusB a :T }";
        let mode = ReasoningMode::Disabled;
        after_first(config, mode, first, &shapes, commit, &shapes_reader())
    };
    let (a, b) = (
        run("GRAPH :secret { :q07FocusA :marker :v }"),
        run("GRAPH :secret { :q07FocusB :marker :v }"),
    );
    assert!(a.detail.contains("q07Focus"), "{}", a.detail);
    same_for_both(a, b);
}

/// R1-D1 (DL): the committer's graphs have two minimal inconsistent sets (`:x` in `:A` and
/// `:B`, or in `:A` and `:C`, each pair disjoint); which one is shown doesn't depend on
/// which of `:B` and `:C` a secret statement made the dictionary know first.
#[test]
fn dl_justification_shown_doesnt_follow_term_ids() {
    let run = |first: &str| {
        let data = "GRAPH :open { :A owl:disjointWith :B . :A owl:disjointWith :C }";
        let commit = "GRAPH :open { :x a :A, :B, :C }";
        let (config, mode) = (StoreConfig::in_memory(), ReasoningMode::Owl2Dl);
        after_first(config, mode, first, data, commit, &open_reader())
    };
    let (b, c) = (
        run("GRAPH :secret { :B :marker :v }"),
        run("GRAPH :secret { :C :marker :v }"),
    );
    assert!(b.detail.contains("OWL 2 DL"), "{}", b.detail);
    same_for_both(b, c);
}

/// A shapes graph `:SECRET_*` names, with a one-member `sh:or` list, and a committer that
/// may read `:open` only but write the shapes graph: it adds a second head to the list.
fn shapes_refusal(secret: &str, requester: &Requester) -> String {
    let config = StoreConfig::in_memory();
    let shapes = config.shapes_graph.clone();
    let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let p = MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    );
    let data = format!(
        "GRAPH <{shapes}> {{ :{secret} sh:targetNode :x ; sh:or :L . \
         :L <{rdf}first> :OK ; <{rdf}rest> <{rdf}nil> }}"
    );
    p.apply(update(&data), &Requester::all(), &MutationTicket::new())
        .expect("valid shapes");
    let commit = format!("GRAPH <{shapes}> {{ :L <{rdf}first> :OTHER }}");
    match p.apply(update(&commit), requester, &MutationTicket::new()) {
        Err(MutationError::Store { source, .. }) => source.to_string(),
        other => panic!("must refuse the shapes: {other:?}"),
    }
}

/// May read `:open`, and write `:open` and the shapes graph.
fn shapes_writer() -> Requester {
    let read = Arc::new(GraphAccess {
        graphs: vec!["http://example.com/open".into()],
        ..GraphAccess::default()
    });
    let write = Arc::new(GraphAccess {
        graphs: vec![
            "http://example.com/open".into(),
            StoreConfig::in_memory().shapes_graph,
        ],
        ..GraphAccess::default()
    });
    Requester::new(ReadScope::Graphs(read), WriteScope::Graphs(write))
}

/// R1-D2: the problems of a shapes graph the committer may not read don't name its shapes.
#[test]
fn shapes_refusal_doesnt_describe_an_unreadable_shapes_graph() {
    let (a, b) = (
        shapes_refusal("SECRET_A", &shapes_writer()),
        shapes_refusal("SECRET_B", &shapes_writer()),
    );
    assert!(!a.contains("SECRET"), "{a}");
    assert_eq!(a, b);
}

/// Control: a committer that may read every graph is told which shape.
#[test]
fn shapes_refusal_full_reader_names_the_shape() {
    let refusal = shapes_refusal("SECRET_A", &Requester::all());
    assert!(refusal.contains("SECRET_A"), "{refusal}");
}

// Fix check r1 (R1-D1 still open): every term is compared whole, a blank node by its
// label (the store's name for it, in the committer's own view), so the shown focus or
// individual is the least of the view's, whatever was interned first.

/// A readable shape requiring `:p` of every `:T`, then `first` (a statement deciding only
/// which term is interned first), then `data`; `commit` by a reader of `:open` and the
/// shapes graph. The SHACL gate enforces, without reasoning.
fn shacl_after(first: &str, data: &str, commit: MutationCommand) -> MutationReject {
    let config = StoreConfig {
        shacl_gate: ShaclGate::Enforce(GateSeverity::Violation),
        ..StoreConfig::in_memory()
    };
    let shapes = format!(
        "GRAPH <{}> {{ :S a sh:NodeShape ; sh:targetClass :T ; sh:property _:ps . \
         _:ps sh:path :p ; sh:minCount 1 }}",
        config.shapes_graph
    );
    let mut setup = vec![trig(first), trig(&shapes)];
    if !data.is_empty() {
        setup.push(trig(data));
    }
    rejected_after(
        config,
        ReasoningMode::Disabled,
        &setup,
        commit,
        &shapes_reader(),
    )
}

/// Two named focus nodes whose IRIs differ only after a `_:`: the least is shown, whichever
/// a secret statement made the dictionary know first.
#[test]
fn shacl_named_focus_differing_after_blank_marker_is_the_least() {
    let (a, b) = ("<http://example.com/_:A>", "<http://example.com/_:B>");
    let commit = update(&format!("GRAPH :open {{ {a} a :T . {b} a :T }}"));
    for first in [a, b] {
        let reject = shacl_after(
            &format!("GRAPH :secret {{ {first} :marker :v }}"),
            "",
            commit.clone(),
        );
        let focus = reject.explanation.expect("explained").focus_resource;
        assert_eq!(focus, a, "interned first: {first}");
    }
}

/// Two blank focus nodes, each anchored by a readable label: the least label is shown,
/// whichever a secret statement made the dictionary know first.
#[test]
fn shacl_anchored_blank_focus_is_the_least() {
    let anchors = "GRAPH :open { _:q07a rdfs:label \"A\" . _:q07b rdfs:label \"B\" }";
    let commit = trig("GRAPH :open { _:q07a a :T . _:q07b a :T }");
    for first in ["_:q07a", "_:q07b"] {
        let reject = shacl_after(
            &format!("GRAPH :secret {{ {first} :marker :v }}"),
            anchors,
            commit.clone(),
        );
        let focus = reject.explanation.expect("explained").focus_resource;
        assert_eq!(focus, "_:q07a", "interned first: {first}");
    }
}

/// Two anonymous individuals, each anchored by a readable label and each in both of two
/// disjoint classes: the justification shown names one of them, the same one whichever a
/// secret statement made the dictionary know first.
#[test]
fn dl_anchored_blank_individual_doesnt_follow_term_ids() {
    let data = "GRAPH :open { :C1 owl:disjointWith :C2 . \
         _:q07a rdfs:label \"A\" . _:q07b rdfs:label \"B\" }";
    let commit = trig("GRAPH :open { _:q07a a :C1, :C2 . _:q07b a :C1, :C2 }");
    let run = |first: &str| {
        rejected_after(
            StoreConfig::in_memory(),
            ReasoningMode::Owl2Dl,
            &[
                trig(&format!("GRAPH :secret {{ {first} :marker :v }}")),
                trig(data),
            ],
            commit.clone(),
            &open_reader(),
        )
        .explanation
        .expect("explained")
        .evidence
    };
    let (a, b) = (run("_:q07a"), run("_:q07b"));
    let named = |label: &str| a.iter().any(|e| e.subject == label);
    assert!(named("_:q07a") != named("_:q07b"), "one individual: {a:#?}");
    assert_eq!(a, b);
}
