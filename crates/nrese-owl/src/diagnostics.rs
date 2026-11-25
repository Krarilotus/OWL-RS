//! What isn't well-formed OWL 2 DL in an ontology read from RDF: reported with the terms
//! involved, never dropped silently. A task an affected axiom takes part in is answered
//! `unsupported` (docs/design/owl2-dl.md §2).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use crate::mapping::{Ontology, Terms};
use crate::model::{Axiom, Characteristic, ClassExpr, EntityKind, Interner, ObjProp, Term};

/// What the store does with a part of the data that breaks the premise of the OWL 2 RL rules'
/// closure (theorem PR1, OWL 2 Profiles §4.3: the graph is exactly the mapping of the ontology
/// read), per kind ([`Treatments`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Treatment {
    /// No model of the read ontology says anything of the source ([`Diagnostic::is_fatal`]):
    /// off the rules route, no answer drawn from the rules told sound, consistency unknown.
    Fatal,
    /// Off the rules route with its reason ([`Diagnostic::reads_otherwise`]): no answer drawn
    /// from the rules told sound, and a clash only the rules find undecided; the read
    /// ontology's consistency stays decided, but for a name of two kinds an axiom reads
    /// ([`Diagnostic::joins_kinds`]).
    OffRoute,
    /// Counted, the route kept.
    Counted,
}

/// A [`Treatment`] that never keeps the route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strict {
    Fatal,
    OffRoute,
}

impl From<Strict> for Treatment {
    fn from(strict: Strict) -> Self {
        match strict {
            Strict::Fatal => Self::Fatal,
            Strict::OffRoute => Self::OffRoute,
        }
    }
}

/// The [`Treatment`] of each kind that breaks the rules' premise and needn't be fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Treatments {
    /// A name of two kinds that an axiom other than an assertion names in one of them
    /// ([`Diagnostic::TwoKinds`] `by_axiom`). Either way off the route, with its consistency
    /// unknown: the DL engines read the name as one ([`Diagnostic::joins_kinds`]), so no
    /// verdict of theirs, consistent or inconsistent, speaks of the source. `Fatal` adds what
    /// [`Diagnostic::is_fatal`] adds: a part counted as left out, which no task certifies.
    /// Default fatal: the rules apply that axiom to the other kind's statements, so a model of
    /// the read axioms certifies nothing of the source.
    pub two_kinds_by_axiom: Strict,
    /// A name of two kinds used only in assertions and statements read past. Default counted:
    /// only the rules for equality read the name, and they copy an object or a data assertion
    /// as OWL 2 DL entails it.
    pub two_kinds_bare: Treatment,
    /// A structure no axiom uses that the rules may read more of than one structure says
    /// ([`Diagnostic::Unused`] `reads_more`). Default off the route: under OWL 2 DL an unused
    /// structure says nothing, so the read ontology's consistency stands.
    pub unused_reads_more: Strict,
}

impl Default for Treatments {
    fn default() -> Self {
        Self {
            two_kinds_by_axiom: Strict::Fatal,
            two_kinds_bare: Treatment::Counted,
            unused_reads_more: Strict::OffRoute,
        }
    }
}

impl Ontology {
    /// Gives each name of two kinds and each unused structure its kind's treatment (the readers
    /// give the defaults).
    pub fn treat(&mut self, treatments: &Treatments) {
        for d in &mut self.diagnostics {
            if let Diagnostic::TwoKinds {
                by_axiom,
                treatment,
                ..
            } = d
            {
                *treatment = if *by_axiom {
                    treatments.two_kinds_by_axiom.into()
                } else {
                    treatments.two_kinds_bare
                };
            } else if let Diagnostic::Unused {
                reads_more: true,
                treatment,
                ..
            } = d
            {
                *treatment = treatments.unused_reads_more.into();
            }
        }
    }
}

/// A problem found while reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Diagnostic {
    /// A structure that doesn't match the mapping.
    Malformed { node: Term, what: &'static str },
    /// A blank node used by two expressions (OWL 2 allows one).
    SharedBlankNode { node: Term },
    /// A list that doesn't end in `rdf:nil`, has a cell twice, or a cell without
    /// `rdf:first` and `rdf:rest`.
    BrokenList { head: Term },
    /// A property used without a declaration (typed by its use).
    UndeclaredProperty { property: Term },
    /// A name used as two kinds OWL 2 DL keeps apart (*OWL 2 Structural Specification* §5.8.1:
    /// two of object, data and annotation property, or class and datatype;
    /// [`crate::typing::two_kinds`]). `by_axiom`: an axiom other than an assertion names it in
    /// one of them. `treatment`: what the store does with it ([`Treatments`]).
    TwoKinds {
        term: Term,
        kinds: [EntityKind; 2],
        by_axiom: bool,
        treatment: Treatment,
    },
    /// OWL 2 syntax NRESE doesn't read yet.
    Unsupported { node: Term, what: &'static str },
    /// OWL vocabulary used outside any form of the mapping.
    NotOwl { triple: [Term; 3] },
    /// A structure (an expression, a list) no axiom uses: it says nothing under the
    /// direct semantics (OWL 1 conclusions state expressions so). `reads_more`: the OWL 2 RL
    /// rules may read more of it than one structure says: it has two forms, a negative
    /// property assertion's statements untyped, or is also an individual of an axiom (which
    /// an equality can give a second structure's statements). `treatment`: what the store does
    /// with it ([`Treatments`]); counted where it doesn't read more.
    Unused {
        node: Term,
        reads_more: bool,
        treatment: Treatment,
    },
    /// A global restriction of OWL 2 DL broken: a non-simple property where only simple
    /// ones may be (cardinalities, `Self`, functional, inverse-functional, irreflexive,
    /// asymmetric, disjoint properties). `axiom` indexes [`Ontology::axioms`].
    NonSimpleProperty {
        property: Term,
        axiom: usize,
        what: &'static str,
    },
    /// An impossible pair in the strict property order required by OWL 2 DL (§11.2).
    IrregularPropertyHierarchy { lower: ObjProp, upper: ObjProp },
    /// A global restriction of OWL 2 DL broken: an anonymous individual in a nominal or a
    /// `hasValue`, which take named ones only (*OWL 2 Structural Specification* §11.2).
    /// The Direct Semantics still reads it (§2.3.7–2.4): in an ontology, one individual,
    /// existential over the whole ontology, which keys skip as they skip every anonymous
    /// one ([`Diagnostic::is_fatal`]); in a conclusion, as every anonymous individual
    /// there, one to prove.
    AnonymousIndividual {
        node: Term,
        what: &'static str,
        axioms: [Option<usize>; 2],
    },
    /// A retained axiom that witnesses a violation of OWL 2 DL's global restrictions.
    GlobalRestriction {
        term: Term,
        axiom: usize,
        what: &'static str,
    },
    /// Text the functional-syntax reader can't read (`crate::ofn`), where it is (from 1;
    /// the column in characters): the axiom it is in is left out.
    Syntax {
        line: u32,
        column: u32,
        what: &'static str,
    },
    /// A disjointness with a repeated operand. The canonical reading deduplicates it
    /// when at least two operands remain. With one operand, and under the RDF-Based
    /// reading, a repeat expresses self-disjointness.
    RepeatedOperand,
    /// An IRI of the reserved vocabulary used as a class, property or individual (*OWL 2
    /// Structural Specification* §2.4): the axioms that do are left out.
    Reserved { term: Term },
    /// A DL-safe rule (SWRL, not OWL 2: *OWL 2 Structural Specification* §9 has no rule
    /// axiom), kept in [`Ontology::rules`] at this index.
    Rule { rule: usize },
}

impl Diagnostic {
    /// What makes two reports the same one: a statement outside the mapping by its whole
    /// triple, any other by one term.
    pub(crate) fn key(&self) -> ([Term; 3], &'static str) {
        let (term, what) = match *self {
            Self::Malformed { node, what } => (node, what),
            Self::SharedBlankNode { node } => (node, "shared"),
            Self::BrokenList { head } => (head, "list"),
            Self::UndeclaredProperty { property } => (property, "undeclared"),
            Self::TwoKinds { term, .. } => (term, "two kinds"),
            Self::Unsupported { node, what } => (node, what),
            Self::NotOwl { triple } => return (triple, "not owl"),
            Self::Unused { node, .. } => (node, "unused"),
            Self::NonSimpleProperty { property, what, .. } => (property, what),
            Self::IrregularPropertyHierarchy { upper, .. } => (upper.named(), "irregular roles"),
            Self::AnonymousIndividual { node, what, .. } => (node, what),
            Self::GlobalRestriction { term, what, .. } => (term, what),
            Self::Syntax { line, column, what } => {
                ((u64::from(line) << 32) | u64::from(column), what)
            }
            Self::Rule { rule } => (rule as Term, "rule"),
            Self::RepeatedOperand => (0, "repeated operand"),
            Self::Reserved { term } => (term, "reserved"),
        };
        ([term, 0, 0], what)
    }

    /// Whether reasoning over the ontology may go wrong because of it. Not for what OWL 2
    /// DL forbids but whose meaning is unambiguous: an undeclared property (typed by its
    /// use), a blank node shared by expressions (OWL 1 ontologies do it; the expression
    /// is the same), an anonymous individual in a nominal or `hasValue`. A name of two kinds
    /// and a structure no axiom uses are fatal where their [`Treatment`] says so (a structure
    /// no axiom uses says nothing; by default a name of two kinds an axiom reads is fatal).
    ///
    /// *OWL 2 Structural Specification* §11.2 keeps anonymous individuals out of OWL 2 DL in
    /// more places: in a nominal or `hasValue`, in
    /// `SameIndividual`, `DifferentIndividuals` or a negative assertion, and in assertions
    /// whose anonymous individuals don't form its forest. None of them is fatal: why the
    /// answers on such input stay those of the Direct Semantics, and what witnesses it, is in
    /// docs/design/owl2-dl.md §16 ("Anonymous individuals where §11.2 keeps them out").
    pub fn is_fatal(&self) -> bool {
        match self {
            Self::UndeclaredProperty { .. }
            | Self::SharedBlankNode { .. }
            | Self::RepeatedOperand
            | Self::AnonymousIndividual { .. } => false,
            Self::TwoKinds { treatment, .. } | Self::Unused { treatment, .. } => {
                *treatment == Treatment::Fatal
            }
            Self::Malformed { .. }
            | Self::BrokenList { .. }
            | Self::Unsupported { .. }
            | Self::NotOwl { .. }
            | Self::NonSimpleProperty { .. }
            | Self::IrregularPropertyHierarchy { .. }
            | Self::GlobalRestriction { .. }
            | Self::Syntax { .. }
            | Self::Reserved { .. }
            | Self::Rule { .. } => true,
        }
    }

    /// Whether the reader left an axiom out whose statements the OWL 2 RL rules may still
    /// read (a reserved name, OWL outside the mapping, a broken structure, syntax not read).
    pub fn leaves_out(&self) -> bool {
        matches!(
            self,
            Self::Reserved { .. }
                | Self::NotOwl { .. }
                | Self::Malformed { .. }
                | Self::BrokenList { .. }
                | Self::Unsupported { .. }
        )
    }

    /// Whether the OWL 2 RL rules read the part otherwise than OWL 2 DL reads the ontology,
    /// which breaks the premise of their closure (theorem PR1, OWL 2 Profiles §4.3): a name
    /// of two kinds or a structure no axiom uses, unless its [`Treatment`] only counts it.
    pub fn reads_otherwise(&self) -> bool {
        match self {
            Self::TwoKinds { treatment, .. } | Self::Unused { treatment, .. } => {
                *treatment != Treatment::Counted
            }
            Self::Malformed { .. }
            | Self::SharedBlankNode { .. }
            | Self::BrokenList { .. }
            | Self::UndeclaredProperty { .. }
            | Self::Unsupported { .. }
            | Self::NotOwl { .. }
            | Self::NonSimpleProperty { .. }
            | Self::IrregularPropertyHierarchy { .. }
            | Self::GlobalRestriction { .. }
            | Self::AnonymousIndividual { .. }
            | Self::Syntax { .. }
            | Self::RepeatedOperand
            | Self::Reserved { .. }
            | Self::Rule { .. } => false,
        }
    }

    /// Whether the DL engines read one name where OWL 2 DL keeps two kinds apart: a name of two
    /// kinds an axiom reads ([`Diagnostic::TwoKinds`] `by_axiom`), whatever its [`Treatment`].
    /// The engines then read its uses of both kinds as one name's, so no verdict of theirs on
    /// the read ontology's consistency speaks of the source.
    pub fn joins_kinds(&self) -> bool {
        matches!(self, Self::TwoKinds { by_axiom: true, .. })
    }

    /// Whether the reader leaves out or alters an axiom for it. Then a reasoner over what it
    /// read may miss conclusions of the source, so a claim that rests on reading all of
    /// them, the QL rewriting's `complete`, is lowered, with this diagnostic as its reason
    /// ([`crate::ql::Hazard::LeftOut`]). Each kind is decided in its own arm, with no
    /// catch-all, so a new kind is decided here; one that isn't a hazard says why.
    pub fn alters_axioms(&self) -> bool {
        match self {
            // Left out: the axiom, the statement or the text isn't read.
            Self::Malformed { .. }
            | Self::BrokenList { .. }
            | Self::Unsupported { .. }
            | Self::NotOwl { .. }
            | Self::Syntax { .. }
            | Self::Reserved { .. } => true,
            // Altered where an axiom names it (`by_axiom`): the reader reads the name as one
            // (`joins_kinds`), so an axiom on one kind is read of the other kind's uses too,
            // and a property declared both is read as an object property only. Read as the
            // source states it where only assertions and statements read past use it: each
            // assertion by its value, a literal's as a data one, an individual's as an object
            // one. Its `treatment` is what the store does with it, not what is read.
            Self::TwoKinds { by_axiom, .. } => *by_axiom,
            // Kept beside the ontology (`Ontology::rules`), which a reasoner of OWL 2 alone
            // doesn't apply.
            Self::Rule { .. } => true,
            // Read as the source states it: the expression two axioms share is the same one
            // in each.
            Self::SharedBlankNode { .. } => false,
            // Read as the source states it: typed by its use, as a declaration would type it.
            Self::UndeclaredProperty { .. } => false,
            // No axiom: a structure no axiom uses says nothing under the Direct Semantics,
            // whatever the OWL 2 RL rules may read more of it (`reads_more`: their premise,
            // `reads_otherwise`). Its `treatment` is the store's, not the reader's.
            Self::Unused { .. } => false,
            // Read as written: only a global restriction of OWL 2 DL is broken (what keeps
            // reasoning decidable), not what the axiom says; what a reasoner doesn't follow
            // of it is that reasoner's own hazard.
            Self::NonSimpleProperty { .. }
            | Self::IrregularPropertyHierarchy { .. }
            | Self::GlobalRestriction { .. } => false,
            // Read as written: the Direct Semantics reads it (§2.3.7–2.4), as above.
            Self::AnonymousIndividual { .. } => false,
            // Read according to the selected disjointness semantics.
            Self::RepeatedOperand => false,
        }
    }

    /// What it is, in words, its terms as `name` writes them (the reader has no dictionary).
    pub fn describe(&self, name: &dyn Fn(Term) -> String) -> String {
        let at = |what: &str, term| format!("{what}: {}", name(term));
        match *self {
            Self::Malformed { node, what } => at(what, node),
            Self::SharedBlankNode { node } => at("a blank node two expressions use", node),
            Self::BrokenList { head } => at("a list not ending in rdf:nil, or a bad cell", head),
            Self::UndeclaredProperty { property } => at("a property used undeclared", property),
            Self::TwoKinds {
                term,
                kinds: [a, b],
                ..
            } => at(&format!("a name used as two kinds ({a:?} and {b:?})"), term),
            Self::Unsupported { node, what } => at(&format!("{what}, not read yet"), node),
            Self::NotOwl { triple } => {
                let statement = triple.map(name).join(" ");
                format!("OWL vocabulary outside the mapping: {statement}")
            }
            Self::Unused { node, .. } => at("a structure no axiom uses", node),
            Self::NonSimpleProperty { property, what, .. } => {
                at(&format!("a non-simple property in {what}"), property)
            }
            Self::IrregularPropertyHierarchy { lower, upper } => {
                let property = |p| match p {
                    ObjProp::Named(t) => name(t),
                    ObjProp::Inverse(t) => format!("inverse of {}", name(t)),
                };
                format!(
                    "an irregular property hierarchy: {} < {}",
                    property(lower),
                    property(upper)
                )
            }
            Self::AnonymousIndividual { node, what, .. } => at(what, node),
            Self::GlobalRestriction { term, what, .. } => at(what, term),
            Self::Syntax { line, column, what } => {
                format!("text not read at line {line}, column {column}: {what}")
            }
            Self::RepeatedOperand => "a disjointness that repeats an operand".to_owned(),
            Self::Reserved { term } => at("reserved vocabulary used as an entity", term),
            Self::Rule { rule } => format!("rule {}, a DL-safe rule, not OWL 2", rule + 1),
        }
    }
}

/// Reports the axioms that use non-simple properties where OWL 2 DL requires simple
/// ones. A property is non-simple if a composite one is (through the hierarchy) a
/// subproperty of it; so is its inverse. Composite (*OWL 2 Structural Specification*
/// §11.1): a chain's or a transitive property's superproperty, and
/// `owl:topObjectProperty` and `owl:bottomObjectProperty`. The bottom seed belongs to
/// the stored ontology's membership check.
pub(crate) fn check_global_restrictions(
    ontology: &Ontology,
    membership: bool,
    out: &mut Vec<Diagnostic>,
) {
    if let Some((lower, upper)) = impossible_role_order(ontology) {
        out.push(Diagnostic::IrregularPropertyHierarchy { lower, upper });
    }
    // Nominals and `hasValue` take named individuals only (§11.2).
    let anonymous: BTreeSet<Term> = crate::mapping::in_expressions(ontology)
        .filter(|t| ontology.anonymous.contains(t))
        .collect();
    for node in anonymous {
        out.push(Diagnostic::AnonymousIndividual {
            node,
            what: "a blank node in oneOf or hasValue",
            axioms: [None; 2],
        });
    }
    let mut supers: HashMap<Term, Vec<Term>> = HashMap::new();
    let mut seeds: Vec<Term> = ontology.builtin.top_object.into_iter().collect();
    if membership {
        seeds.extend(ontology.builtin.bottom_object);
    }
    for axiom in &ontology.axioms {
        match axiom {
            Axiom::SubObjectPropertyOf(chain, sup) => {
                if chain.len() > 1 {
                    seeds.push(sup.named());
                } else {
                    supers
                        .entry(chain[0].named())
                        .or_default()
                        .push(sup.named());
                }
            }
            Axiom::EquivalentObjectProperties(properties) => {
                for a in properties {
                    for b in properties {
                        if a != b {
                            supers.entry(a.named()).or_default().push(b.named());
                        }
                    }
                }
            }
            Axiom::InverseObjectProperties(a, b) => {
                supers.entry(a.named()).or_default().push(b.named());
                supers.entry(b.named()).or_default().push(a.named());
            }
            Axiom::ObjectCharacteristic(Characteristic::Transitive, p) => seeds.push(p.named()),
            _ => {}
        }
    }
    let mut non_simple: HashSet<Term> = HashSet::new();
    while let Some(property) = seeds.pop() {
        if non_simple.insert(property) {
            seeds.extend(supers.get(&property).into_iter().flatten().copied());
        }
    }
    if non_simple.is_empty() {
        return;
    }
    let simple = |p: &ObjProp| !non_simple.contains(&p.named());
    // Class expressions that need a simple property.
    let mut needing: HashMap<u32, (Term, &'static str)> = HashMap::new();
    for id in 0..ontology.classes.len() as u32 {
        let (property, what) = match ontology.classes.get(id) {
            ClassExpr::Min(_, p, _) | ClassExpr::Max(_, p, _) | ClassExpr::Exact(_, p, _) => {
                (*p, "a cardinality restriction")
            }
            ClassExpr::HasSelf(p) => (*p, "a self restriction"),
            _ => continue,
        };
        if !simple(&property) {
            needing.insert(id, (property.named(), what));
        }
    }
    let mut found = Vec::new();
    let mut memo = (!needing.is_empty()).then(|| vec![None; ontology.classes.len()]);
    for (index, axiom) in ontology.axioms.iter().enumerate() {
        let direct = match axiom {
            Axiom::ObjectCharacteristic(
                kind @ (Characteristic::Functional
                | Characteristic::InverseFunctional
                | Characteristic::Irreflexive
                | Characteristic::Asymmetric),
                p,
            ) if !simple(p) => Some((
                p.named(),
                match kind {
                    Characteristic::Functional => "a functional property",
                    Characteristic::InverseFunctional => "an inverse-functional property",
                    Characteristic::Irreflexive => "an irreflexive property",
                    _ => "an asymmetric property",
                },
            )),
            Axiom::DisjointObjectProperties(properties) => properties
                .iter()
                .find(|p| !simple(p))
                .map(|p| (p.named(), "disjoint properties")),
            _ => None,
        };
        if let Some((property, what)) = direct {
            found.push(Diagnostic::NonSimpleProperty {
                property,
                axiom: index,
                what,
            });
            continue;
        }
        if let Some(memo) = memo.as_deref_mut() {
            for expr in expressions_of(axiom) {
                if let Some((property, what)) = uses_any(ontology, expr, &needing, memo) {
                    found.push(Diagnostic::NonSimpleProperty {
                        property,
                        axiom: index,
                        what,
                    });
                    break;
                }
            }
        }
    }
    out.extend(found);
}

/// The first impossible pair in the least strict order required by §11.2.
fn impossible_role_order(ontology: &Ontology) -> Option<(ObjProp, ObjProp)> {
    let top = ontology.builtin.top_object.map(ObjProp::Named);
    let mut pairs = VecDeque::new();
    for axiom in &ontology.axioms {
        let Axiom::SubObjectPropertyOf(chain, upper) = axiom else {
            continue;
        };
        if chain.len() < 2 || Some(*upper) == top {
            continue;
        }
        let last = chain.len() - 1;
        let required = match (chain[0] == *upper, chain[last] == *upper) {
            (true, true) if last == 1 => continue,
            (true, true) => return Some((*upper, *upper)),
            (true, false) => &chain[1..],
            (false, true) => &chain[..last],
            (false, false) => chain,
        };
        pairs.extend(required.iter().map(|&lower| (lower, *upper)));
    }
    if pairs.is_empty() {
        return None;
    }

    let mut seen = HashSet::new();
    pairs.retain(|&pair| seen.insert(pair));
    let mut before: BTreeMap<ObjProp, Vec<ObjProp>> = BTreeMap::new();
    let mut after: HashMap<ObjProp, Vec<ObjProp>> = HashMap::new();
    while let Some((lower, upper)) = pairs.pop_front() {
        if lower == upper {
            return Some((lower, upper));
        }
        let predecessors = before
            .get(&lower)
            .into_iter()
            .flatten()
            .map(|&p| (p, upper));
        let successors = after.get(&upper).into_iter().flatten().map(|&s| (lower, s));
        let inverted = matches!(upper, ObjProp::Named(_)).then(|| (lower.inverse(), upper));
        pairs.extend(
            predecessors
                .chain(successors)
                .chain(inverted)
                .filter(|&p| seen.insert(p)),
        );
        before.entry(upper).or_default().push(lower);
        after.entry(lower).or_default().push(upper);
    }

    // Test against the oriented property hierarchy, separately from the strict order.
    let mut hierarchy: HashMap<ObjProp, Vec<ObjProp>> = HashMap::new();
    let mut include = |lower: ObjProp, upper: ObjProp| {
        for (a, b) in [(lower, upper), (lower.inverse(), upper.inverse())] {
            hierarchy.entry(a).or_default().push(b);
        }
    };
    for axiom in &ontology.axioms {
        match axiom {
            Axiom::SubObjectPropertyOf(chain, upper) if chain.len() == 1 => {
                include(chain[0], *upper)
            }
            Axiom::EquivalentObjectProperties(ps) => {
                for pair in ps.windows(2) {
                    include(pair[0], pair[1]);
                    include(pair[1], pair[0]);
                }
            }
            Axiom::InverseObjectProperties(a, b) => {
                include(*a, b.inverse());
                include(b.inverse(), *a);
            }
            Axiom::ObjectCharacteristic(Characteristic::Symmetric, p) => include(*p, p.inverse()),
            _ => {}
        }
    }
    let mut reached = HashSet::new();
    let mut todo = Vec::new();
    for upper in before.into_keys() {
        reached.clear();
        todo.push(upper);
        while let Some(lower) = todo.pop() {
            if !reached.insert(lower) {
                continue;
            }
            if seen.contains(&(lower, upper)) {
                return Some((lower, upper));
            }
            todo.extend(hierarchy.get(&lower).into_iter().flatten().copied());
        }
    }
    None
}

/// The class expressions an axiom holds directly.
fn expressions_of(axiom: &Axiom) -> Vec<u32> {
    let mut out = Vec::new();
    push_expressions(axiom, &mut out);
    out
}

/// [`expressions_of`], pushed onto `out`.
fn push_expressions(axiom: &Axiom, out: &mut Vec<u32>) {
    match axiom {
        Axiom::SubClassOf(a, b) => out.extend([a.0, b.0]),
        Axiom::EquivalentClasses(xs) | Axiom::DisjointClasses(xs) => {
            out.extend(xs.iter().map(|x| x.0))
        }
        Axiom::DisjointUnion(c, xs) => {
            out.push(c.0);
            out.extend(xs.iter().map(|x| x.0));
        }
        Axiom::ObjectPropertyDomain(_, x)
        | Axiom::ObjectPropertyRange(_, x)
        | Axiom::DataPropertyDomain(_, x)
        | Axiom::HasKey(x, _, _)
        | Axiom::ClassAssertion(x, _) => out.push(x.0),
        _ => {}
    }
}

fn class_children(expr: &ClassExpr) -> &[crate::ExprId] {
    match expr {
        ClassExpr::And(xs) | ClassExpr::Or(xs) => xs,
        ClassExpr::Not(x)
        | ClassExpr::Some(_, x)
        | ClassExpr::All(_, x)
        | ClassExpr::Min(_, _, x)
        | ClassExpr::Max(_, _, x)
        | ClassExpr::Exact(_, _, x) => std::slice::from_ref(x),
        _ => &[],
    }
}

/// The first expression in `id`'s subexpressions that `needing` names, memoised across axioms.
// Acyclic: Interner::intern is append-only and immutable (model.rs:211-219), and the RDF
// reader guards against cyclic blank-node class expressions (mapping.rs:1205-1210).
fn uses_any(
    ontology: &Ontology,
    id: u32,
    needing: &HashMap<u32, (Term, &'static str)>,
    memo: &mut [Option<Option<(Term, &'static str)>>],
) -> Option<(Term, &'static str)> {
    let mut stack = vec![(id, false)];
    while let Some((cur, post)) = stack.pop() {
        if post {
            let hit = class_children(ontology.classes.get(cur))
                .iter()
                .find_map(|&c| memo[c.0 as usize].flatten());
            memo[cur as usize] = Some(hit);
            continue;
        }
        if memo[cur as usize].is_some() {
            continue;
        }
        #[cfg(any(test, all(feature = "witness", debug_assertions)))]
        NON_SIMPLE_EXPANSIONS.with(|c| c.set(c.get() + 1));
        if let Some(&hit) = needing.get(&cur) {
            memo[cur as usize] = Some(Some(hit));
            continue;
        }
        memo[cur as usize] = Some(None);
        let kids = class_children(ontology.classes.get(cur));
        if !kids.is_empty() {
            stack.push((cur, true));
            stack.extend(kids.iter().map(|c| (c.0, false)));
        }
    }
    memo[id as usize].flatten()
}

#[cfg(any(test, all(feature = "witness", debug_assertions)))]
thread_local! {
    static NON_SIMPLE_EXPANSIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Class-expression expansions made by the non-simple-property check on this thread.
#[cfg(any(test, all(feature = "witness", debug_assertions)))]
pub fn non_simple_expansions() -> u64 {
    NON_SIMPLE_EXPANSIONS.with(std::cell::Cell::get)
}

/// Whether an IRI is of the reserved vocabulary (`rdf:`, `rdfs:`, `xsd:`, `owl:`) and not
/// one of the names OWL 2 DL gives it (`owl:Thing`, `owl:Nothing`, the top and bottom
/// properties): a class or property the reader leaves out (`leave_out_reserved`); where an
/// individual goes, [`reserved_as_individual`]. Callers that choose input for the reader ask
/// it here, not by a copy of the list.
pub fn reserved_name(iri: &str) -> bool {
    const ALLOWED: [&str; 6] = [
        "Thing",
        "Nothing",
        "topObjectProperty",
        "bottomObjectProperty",
        "topDataProperty",
        "bottomDataProperty",
    ];
    let allowed = iri
        .strip_prefix(crate::vocab::OWL)
        .is_some_and(|l| ALLOWED.contains(&l));
    reserved_vocabulary(iri) && !allowed
}

/// Whether an IRI where an individual goes is one the reader leaves out: every IRI of the
/// reserved vocabulary, the six names [`reserved_name`] allows included, which OWL 2 DL
/// gives a class's or a property's role only (*Structural Specification* §2.4, §5.6.1).
pub fn reserved_as_individual(iri: &str) -> bool {
    reserved_vocabulary(iri)
}

/// Whether an IRI is of the reserved vocabulary (`rdf:`, `rdfs:`, `xsd:`, `owl:`; §2.4).
pub fn reserved_vocabulary(iri: &str) -> bool {
    use crate::vocab::{OWL, RDF, RDFS, XSD};
    [OWL, RDF, RDFS, XSD].iter().any(|ns| iri.starts_with(ns))
}

/// Leaves out the axioms that use an IRI of the reserved vocabulary (`rdf:`, `rdfs:`,
/// `xsd:`, `owl:`; *OWL 2 Structural Specification* §2.4) as a class, a property or an
/// individual, but for those §2.4 gives OWL 2 DL as a class or a property (`owl:Thing`,
/// `owl:Nothing`, the top and bottom properties; [`reserved_name`] and
/// [`reserved_as_individual`]; datatypes are data ranges, not touched here): every such name an
/// axiom uses, each with a [`Diagnostic::Reserved`] (not only its first: what is reported
/// doesn't depend on the order the statements are read in). Such a name means in OWL 2
/// DL nothing of what it means in RDF (`rdfs:Class`, `rdf:type`, `rdf:first` as entities
/// belong to OWL 2 Full), so
/// reading it as a fresh name would decide what the input doesn't say; left out, what
/// follows from the rest still follows. Terms whose IRI the source doesn't give
/// ([`Terms::iri_text`]) aren't checked.
pub(crate) fn leave_out_reserved(ontology: &mut Ontology, terms: &dyn Terms) {
    // Per term: reserved as a class or property, and as an individual.
    let mut cache: HashMap<Term, [bool; 2], foldhash::fast::RandomState> = HashMap::default();
    let mut reserved = |t: Term, individual: bool| -> bool {
        let roles = *cache.entry(t).or_insert_with(|| reserved_roles(terms, t));
        roles[usize::from(individual)]
    };
    let mut walk = Walk::new(ontology.classes.len());
    let mut kept = Vec::with_capacity(ontology.axioms.len());
    let mut sources = Vec::with_capacity(ontology.axioms.len());
    let mut found = Vec::new();
    // Each reserved name reported once, those reported before included.
    let mut met: HashSet<Term> = (ontology.diagnostics.iter())
        .filter_map(|d| match *d {
            Diagnostic::Reserved { term } => Some(term),
            _ => None,
        })
        .collect();
    let axioms = std::mem::take(&mut ontology.axioms);
    let all_sources = std::mem::take(&mut ontology.sources);
    for (axiom, source) in axioms.into_iter().zip(all_sources) {
        let mut any = false;
        let (names, individuals) = entities_of(&ontology.classes, &axiom, &mut walk);
        let roles =
            (names.iter().map(|&t| (t, false))).chain(individuals.iter().map(|&t| (t, true)));
        for (term, individual) in roles {
            if reserved(term, individual) {
                any = true;
                if met.insert(term) {
                    found.push(Diagnostic::Reserved { term });
                }
            }
        }
        if !any {
            kept.push(axiom);
            sources.push(source);
        }
    }
    ontology.axioms = kept;
    ontology.sources = sources;
    ontology.diagnostics.extend(found);
}

/// Whether `t`'s IRI is reserved as a class or property ([`reserved_name`]), and where an
/// individual goes ([`reserved_as_individual`]); neither where the source gives no IRI.
fn reserved_roles(terms: &dyn Terms, t: Term) -> [bool; 2] {
    (terms.iri_text(t)).map_or([false; 2], |iri| {
        [reserved_name(&iri), reserved_as_individual(&iri)]
    })
}

/// The reserved names that the axioms one statement is read as name in their places, each
/// once, as [`leave_out_reserved`] asks of each axiom. A reader asks it of a disjointness or
/// distinctness that repeats an operand, which it reads as several axioms (the n-ary one and
/// the repeat's own, `model::Disjoint::axioms`), and leaves them all out where one names any:
/// left out one by one, the repeat's axiom alone (`ex:a` different from itself) would stay,
/// a part of a statement read where the rest is not.
/// `walk` is the reader's, kept across its statements.
pub(crate) fn reserved_in(
    classes: &Interner<ClassExpr>,
    axioms: &[Axiom],
    terms: &dyn Terms,
    walk: &mut Walk,
) -> Vec<Term> {
    let mut found = Vec::new();
    for axiom in axioms {
        let (names, individuals) = entities_of(classes, axiom, walk);
        let roles =
            (names.iter().map(|&t| (t, false))).chain(individuals.iter().map(|&t| (t, true)));
        for (term, individual) in roles {
            if reserved_roles(terms, term)[usize::from(individual)] && !found.contains(&term) {
                found.push(term);
            }
        }
    }
    found
}

/// Reports each reserved name a nominal or a `hasValue` puts where an individual goes, which
/// the reader read no axiom of, once: an expression stays interned where the axiom using it is
/// left out, or the structure around it rejected after it was read (a disjoint union of one
/// class), and [`Ontology::named_individuals`], which the DL engines realise, leaves out what
/// is reported so.
pub(crate) fn report_reserved_in_expressions(ontology: &mut Ontology, terms: &dyn Terms) {
    let mut met: HashSet<Term> = (ontology.diagnostics.iter())
        .filter_map(|d| match *d {
            Diagnostic::Reserved { term } => Some(term),
            _ => None,
        })
        .collect();
    let found: Vec<Term> = crate::mapping::in_expressions(ontology)
        .filter(|&t| reserved_roles(terms, t)[1] && met.insert(t))
        .collect();
    (ontology.diagnostics).extend(found.into_iter().map(|term| Diagnostic::Reserved { term }));
}

/// [`entities_of`]'s lists, kept from one axiom to the next so that the walk over every
/// axiom allocates nothing per axiom: the classes and properties found, the individuals found,
/// the expressions left to visit, and per expression the round of the axiom that last visited
/// it.
pub(crate) struct Walk {
    names: Vec<Term>,
    individuals: Vec<Term>,
    stack: Vec<u32>,
    seen: Vec<u32>,
    round: u32,
}

impl Walk {
    /// A walk over an ontology of `expressions` class expressions.
    pub(crate) fn new(expressions: usize) -> Self {
        Self {
            names: Vec::new(),
            individuals: Vec::new(),
            stack: Vec::new(),
            seen: vec![0; expressions],
            round: 0,
        }
    }
}

/// The classes and properties an axiom names, then the individuals (datatypes, literals and
/// facets aside), in `walk`'s lists; its class expressions in `classes`.
fn entities_of<'w>(
    classes: &Interner<ClassExpr>,
    axiom: &Axiom,
    walk: &'w mut Walk,
) -> (&'w [Term], &'w [Term]) {
    use crate::model::EntityKind;
    if walk.round == u32::MAX {
        walk.seen.fill(0);
        walk.round = 0;
    }
    walk.round += 1;
    let Walk {
        names: out,
        individuals: ind,
        stack,
        seen,
        round,
    } = walk;
    out.clear();
    ind.clear();
    stack.clear();
    let p = |q: &ObjProp| q.named();
    match axiom {
        Axiom::Declaration(EntityKind::NamedIndividual, t) => ind.push(*t),
        Axiom::Declaration(kind, t) => {
            // Datatypes and annotation properties of the reserved vocabulary are OWL 2's.
            if !matches!(kind, EntityKind::Datatype | EntityKind::AnnotationProperty) {
                out.push(*t);
            }
        }
        Axiom::SubObjectPropertyOf(chain, sup) => {
            out.extend(chain.iter().map(p));
            out.push(sup.named());
        }
        Axiom::EquivalentObjectProperties(ps) | Axiom::DisjointObjectProperties(ps) => {
            out.extend(ps.iter().map(p));
        }
        Axiom::InverseObjectProperties(a, b2) => out.extend([a.named(), b2.named()]),
        Axiom::ObjectPropertyDomain(q, _)
        | Axiom::ObjectPropertyRange(q, _)
        | Axiom::ObjectCharacteristic(_, q) => out.push(q.named()),
        Axiom::SubDataPropertyOf(a, b2) => out.extend([*a, *b2]),
        Axiom::EquivalentDataProperties(ps) | Axiom::DisjointDataProperties(ps) => {
            out.extend(ps);
        }
        Axiom::DataPropertyDomain(q, _)
        | Axiom::DataPropertyRange(q, _)
        | Axiom::FunctionalDataProperty(q) => out.push(*q),
        Axiom::HasKey(_, os, ds) => {
            out.extend(os.iter().map(p));
            out.extend(ds);
        }
        Axiom::ClassAssertion(_, a) => ind.push(*a),
        Axiom::ObjectPropertyAssertion(q, a, b2)
        | Axiom::NegativeObjectPropertyAssertion(q, a, b2) => {
            out.push(*q);
            ind.extend([*a, *b2]);
        }
        Axiom::DataPropertyAssertion(q, a, _) | Axiom::NegativeDataPropertyAssertion(q, a, _) => {
            out.push(*q);
            ind.push(*a);
        }
        Axiom::SameIndividual(xs) | Axiom::DifferentIndividuals(xs) => ind.extend(xs),
        _ => {}
    }
    push_expressions(axiom, stack);
    while let Some(id) = stack.pop() {
        if seen.len() <= id as usize {
            seen.resize(classes.len(), 0);
        }
        if seen[id as usize] == *round {
            continue;
        }
        seen[id as usize] = *round;
        match classes.get(id) {
            ClassExpr::Class(t) => out.push(*t),
            ClassExpr::And(xs) | ClassExpr::Or(xs) => stack.extend(xs.iter().map(|x| x.0)),
            ClassExpr::Not(x) => stack.push(x.0),
            ClassExpr::OneOf(xs) => ind.extend(xs),
            ClassExpr::Some(q, x)
            | ClassExpr::All(q, x)
            | ClassExpr::Min(_, q, x)
            | ClassExpr::Max(_, q, x)
            | ClassExpr::Exact(_, q, x) => {
                out.push(q.named());
                stack.push(x.0);
            }
            ClassExpr::HasValue(q, a) => {
                out.push(q.named());
                ind.push(*a);
            }
            ClassExpr::HasSelf(q) => out.push(q.named()),
            ClassExpr::DataSome(q, _)
            | ClassExpr::DataAll(q, _)
            | ClassExpr::DataHasValue(q, _)
            | ClassExpr::DataMin(_, q, _)
            | ClassExpr::DataMax(_, q, _)
            | ClassExpr::DataExact(_, q, _) => out.push(*q),
            ClassExpr::Thing | ClassExpr::Nothing => {}
        }
    }
    (out, ind)
}
