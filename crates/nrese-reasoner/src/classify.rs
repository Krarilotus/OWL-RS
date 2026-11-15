//! OWL 2 EL classification: every subsumption between named classes, and the classes
//! that can have no instance.
//!
//! The ontology is read from its RDF triples and normalised as CEL and ELK do: a complex
//! class expression gets a fresh name defined as equivalent to it, n-ary conjunctions
//! become binary ones, property chains of any length become chains of two. Saturation
//! then applies the completion rules (Baader, Brandt and Lutz, *Pushing the EL Envelope*,
//! 2005), per class, until nothing changes:
//!
//! | Rule | If | Then |
//! |---|---|---|
//! | CR1 | `D ∈ S(C)`, `D ⊑ E` | `E ∈ S(C)` |
//! | CR2 | `D1, D2 ∈ S(C)`, `D1 ⊓ D2 ⊑ E` | `E ∈ S(C)` |
//! | CR3 | `D ∈ S(C)`, `D ⊑ ∃r.E` | `C →r E` |
//! | CR4 | `C →r D`, `E ∈ S(D)`, `r ⊑* s`, `∃s.E ⊑ F` | `F ∈ S(C)` |
//! | CR5 | `C →r D`, `⊥ ∈ S(D)` | `⊥ ∈ S(C)` |
//! | CR6 | `C →r D →s E`, `r ⊑* r'`, `s ⊑* s'`, `r' ∘ s' ⊑ t` | `C →t E` |
//!
//! Supported: `rdfs:subClassOf`, `owl:equivalentClass`, `owl:disjointWith`,
//! `owl:AllDisjointClasses`, `owl:intersectionOf`, `owl:someValuesFrom` restrictions,
//! `owl:Thing`, `owl:Nothing`, `rdfs:subPropertyOf`, `owl:equivalentProperty`,
//! `owl:propertyChainAxiom`, `owl:TransitiveProperty`, `rdfs:domain`, and `rdfs:range`
//! (applied where a class axiom creates the edge), between named object properties. Every
//! other class or property axiom is skipped and counted (OWL 2 Profiles §2.2):
//! - outside OWL 2 EL: unions and disjoint unions, universal restrictions, complements,
//!   cardinalities, inverse properties, nominals of more than one individual, disjoint
//!   properties, functional object properties, inverse-functional, irreflexive, symmetric
//!   and asymmetric properties, and the data ranges EL lacks (complements, facet
//!   restrictions);
//! - inside OWL 2 EL but not read here: nominals of one individual (`owl:oneOf` with one
//!   member, `owl:hasValue`), self restrictions, reflexive properties, functional data
//!   properties, restrictions on data properties, EL's data ranges (its datatypes, their
//!   intersections, one-literal enumerations), and the top and bottom properties;
//! - neither, but how the triples state them: named class expressions, datatypes where a
//!   class is read, nodes that state two expressions.
//!
//! The hierarchy is then complete for the part read only. OWL 2's global
//! restrictions are not checked: an irregular role hierarchy (*Structural Specification*
//! §11.2), outside OWL 2 EL as well (Profiles §2.2.6), is read, and CR6 needs no regular one.
//! A data property's domain and range, which say nothing about classes without such a
//! restriction, assertions about individuals, and keys, which constrain only individuals,
//! are not read: an inconsistency among them goes unseen. Nor are an annotation
//! property's domain, range and sub-properties, which say nothing about classes. What a
//! declaration declares is read from nrese-owl's table ([`DECLARATION_KINDS`]), as the
//! mapping reads it.

use std::time::{Duration, Instant};

use hashbrown::{HashMap, HashSet};
use nrese_owl::{DECLARATION_KINDS, EntityKind};
use nrese_xsd::owl::Datatype;

use super::delta::Interrupted;
use super::eval::{OperationStop, Poller};
use super::ir::{OWL, RDF, RDFS, Vocabulary};

type Triple = [u64; 3];

/// A run's stop and the calling thread's countdown to its next poll
/// ([`classify_parallel_within`]): every [`nrese_exec::stop::POLL_UNITS`] units it asks
/// [`OperationStop::fired`]. A unit is counted before it is done.
struct Checkpoint<'a> {
    poll: Poller<'a>,
    stop: &'a OperationStop<'a>,
}

impl Checkpoint<'_> {
    /// One unit of work: `Err` once the stop has fired.
    fn tick(&self) -> Result<(), Interrupted> {
        match self.poll.tick() {
            true => Err(self.stop.interrupted()),
            false => Ok(()),
        }
    }

    /// The closing check of a run's phase: `Err` if the stop has fired, asked now.
    fn finish(&self) -> Result<(), Interrupted> {
        match self.poll.stopped() || self.stop.fired() {
            true => Err(self.stop.interrupted()),
            false => Ok(()),
        }
    }
}

/// A classification's result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Classification {
    /// `(sub, super)` for named classes, `sub != super`; equivalent classes appear both
    /// ways; `owl:Thing` is left out as a superclass.
    pub subsumptions: Vec<(u64, u64)>,
    /// Named classes that can have no instance (subclasses of `owl:Nothing`).
    pub unsatisfiable: Vec<u64>,
    /// Named classes equivalent to `owl:Thing` (superclasses of it): every class is a
    /// subclass of them, which `subsumptions` doesn't repeat for each.
    pub top: Vec<u64>,
    /// Omitted axioms (by predicate and reason), including unsupported expressions,
    /// malformed lists and resource refusals. Their consequences may be missing.
    pub skipped: Vec<(u64, &'static str)>,
}

/// Where a classification spent its time and what it did, for the DL lab
/// (docs/design/owl2-dl-performance.md §5): kept apart from [`Classification`] so equal
/// results compare equal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// Reading the triples into the normalised TBox.
    pub normalise: Duration,
    /// The role closure, chain indexes and ranges.
    pub prepare: Duration,
    pub saturate: Duration,
    /// The result from the subsumer sets.
    pub assemble: Duration,
    /// Concepts of the normalised TBox (named and fresh).
    pub concepts: usize,
    /// Contexts saturated (concepts some rule reached).
    pub contexts: usize,
    /// Subsumers derived, summed over the contexts.
    pub subsumers: usize,
    /// Distinct links `C →r D`.
    pub links: usize,
    /// Conclusions processed (subsumers and links, duplicates included) ...
    pub conclusions: u64,
    /// ... of which were already known.
    pub duplicates: u64,
    pub threads: usize,
}

/// What a predicate of the RDF mapping's class expressions and data ranges is to the reader.
#[derive(Debug, Clone, Copy)]
enum Part {
    /// A constructor EL reads (`owl:intersectionOf`, `owl:someValuesFrom`).
    Read,
    /// A constructor EL doesn't read, and the kind of omission.
    Unread(&'static str),
    /// A constructor's field; with a kind where EL reads no restriction it makes
    /// (`owl:onProperties`).
    Field(Option<&'static str>),
}

/// What a statement is to [`build`], by its predicate, or for `rdf:type` by its object
/// ([`Names::disposition`]).
#[derive(Debug, Clone, Copy)]
enum Disposition {
    /// `rdfs:subClassOf`, `owl:equivalentClass` (both ways), `owl:disjointWith`.
    Class(ClassAxiom),
    /// `rdf:type owl:AllDisjointClasses`.
    AllDisjointClasses,
    /// A property axiom the rules read where the properties are roles.
    Property(PropertyAxiom),
    /// A class or property axiom no rule reads, and its kind.
    Unread(&'static str),
    /// A predicate of the mapping's class expressions and data ranges, its place in their
    /// table: read through the axiom that uses its node; on a named subject it defines that
    /// class (or relates two properties).
    Expression(usize, Part),
}

#[derive(Debug, Clone, Copy)]
enum ClassAxiom {
    Sub,
    Equivalent,
    Disjoint,
}

/// A value by term for terms fixed when the table is made: the reader's own vocabulary's
/// [`Disposition`]s ([`Names`]) and the named datatypes ([`Graph`]). Open-addressed on the
/// term's id and at most half full, so that a lookup, made for every statement or class read,
/// is a multiply, a shift and mostly one comparison.
struct TermTable<V> {
    /// A power of two of slots.
    slots: Box<[Option<(u64, V)>]>,
    shift: u32,
}

impl<V: Copy> FromIterator<(u64, V)> for TermTable<V> {
    /// The last of a term's entries wins.
    fn from_iter<I: IntoIterator<Item = (u64, V)>>(entries: I) -> Self {
        let entries: Vec<(u64, V)> = entries.into_iter().collect();
        let bits = (entries.len() * 2)
            .next_power_of_two()
            .trailing_zeros()
            .max(1);
        let mut table = Self {
            slots: vec![None; 1 << bits].into_boxed_slice(),
            shift: 64 - bits,
        };
        for (term, value) in entries {
            let mut slot = table.slot(term);
            while table.slots[slot].is_some_and(|(held, _)| held != term) {
                slot = (slot + 1) & (table.slots.len() - 1);
            }
            table.slots[slot] = Some((term, value));
        }
        table
    }
}

impl<V: Copy> TermTable<V> {
    /// Where `term` is looked for first (Fibonacci hashing).
    fn slot(&self, term: u64) -> usize {
        (term.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> self.shift) as usize
    }

    fn get(&self, term: u64) -> Option<V> {
        let mut slot = self.slot(term);
        loop {
            match self.slots[slot] {
                Some((held, value)) if held == term => return Some(value),
                Some(_) => slot = (slot + 1) & (self.slots.len() - 1),
                None => return None,
            }
        }
    }
}

struct Names {
    sub_class_of: u64,
    equivalent_class: u64,
    disjoint_with: u64,
    all_disjoint_classes: u64,
    members: u64,
    intersection_of: u64,
    some_values_from: u64,
    on_property: u64,
    first: u64,
    rest: u64,
    nil: u64,
    thing: u64,
    nothing: u64,
    sub_property_of: u64,
    equivalent_property: u64,
    property_chain: u64,
    rdf_type: u64,
    transitive: u64,
    domain: u64,
    range: u64,
    /// Each statement's [`Disposition`] by its predicate, and by its object where its
    /// predicate is `rdf:type`: the one decision of what a statement is, made from one table
    /// of each kind in [`Self::new`].
    by_predicate: TermTable<Disposition>,
    by_type: TermTable<Disposition>,
    /// `owl:topObjectProperty` and the other three, whose meaning no rule has.
    top_and_bottom: [u64; 4],
    /// The datatypes of OWL 2's datatype map.
    datatype_map: Vec<u64>,
    /// What an `rdf:type` object declares its subject as: nrese-owl's table, so that a
    /// declaration is read as the mapping reads it.
    declarations: TermTable<EntityKind>,
}

impl Names {
    fn new(v: &mut impl Vocabulary) -> Self {
        let owl = |v: &mut dyn FnMut(&str) -> u64, local: &str| v(&format!("{OWL}{local}"));
        let mut iri = |text: &str| v.iri(text);
        let mut names = Self {
            sub_class_of: iri(&format!("{RDFS}subClassOf")),
            equivalent_class: owl(&mut iri, "equivalentClass"),
            disjoint_with: owl(&mut iri, "disjointWith"),
            all_disjoint_classes: owl(&mut iri, "AllDisjointClasses"),
            members: owl(&mut iri, "members"),
            intersection_of: owl(&mut iri, "intersectionOf"),
            some_values_from: owl(&mut iri, "someValuesFrom"),
            on_property: owl(&mut iri, "onProperty"),
            first: iri(&format!("{RDF}first")),
            rest: iri(&format!("{RDF}rest")),
            nil: iri(&format!("{RDF}nil")),
            thing: owl(&mut iri, "Thing"),
            nothing: owl(&mut iri, "Nothing"),
            sub_property_of: iri(&format!("{RDFS}subPropertyOf")),
            equivalent_property: owl(&mut iri, "equivalentProperty"),
            property_chain: owl(&mut iri, "propertyChainAxiom"),
            rdf_type: iri(&format!("{RDF}type")),
            transitive: owl(&mut iri, "TransitiveProperty"),
            domain: iri(&format!("{RDFS}domain")),
            range: iri(&format!("{RDFS}range")),
            by_predicate: std::iter::empty().collect(),
            by_type: std::iter::empty().collect(),
            top_and_bottom: [
                "topObjectProperty",
                "bottomObjectProperty",
                "topDataProperty",
                "bottomDataProperty",
            ]
            .map(|local| owl(&mut iri, local)),
            datatype_map: Datatype::ALL.iter().map(|d| iri(&d.iri())).collect(),
            declarations: DECLARATION_KINDS
                .iter()
                .map(|&(class, kind)| (iri(class), kind))
                .collect(),
        };
        // Every predicate of the mapping's class expressions and data ranges (OWL 2 Mapping to
        // RDF Graphs, Tables 1 to 3): the constructors a node has one of, their fields, and
        // what EL makes of each.
        let expression: Vec<(u64, Part)> = [
            ("intersectionOf", Part::Read),
            ("someValuesFrom", Part::Read),
            ("unionOf", Part::Unread("union")),
            ("complementOf", Part::Unread("complement")),
            ("oneOf", Part::Unread("nominal")),
            ("hasValue", Part::Unread("nominal")),
            ("allValuesFrom", Part::Unread("universal restriction")),
            ("hasSelf", Part::Unread("self restriction")),
            ("cardinality", Part::Unread("cardinality")),
            ("minCardinality", Part::Unread("cardinality")),
            ("maxCardinality", Part::Unread("cardinality")),
            ("qualifiedCardinality", Part::Unread("cardinality")),
            ("minQualifiedCardinality", Part::Unread("cardinality")),
            ("maxQualifiedCardinality", Part::Unread("cardinality")),
            ("inverseOf", Part::Unread("inverse property")),
            ("datatypeComplementOf", Part::Unread("data range")),
            ("onDatatype", Part::Unread("data range")),
            ("onProperty", Part::Field(None)),
            ("onClass", Part::Field(None)),
            ("onDataRange", Part::Field(None)),
            ("withRestrictions", Part::Field(None)),
            ("onProperties", Part::Field(Some("n-ary data restriction"))),
        ]
        .into_iter()
        .map(|(local, part)| (owl(&mut iri, local), part))
        .collect();
        // Class and property axioms no rule reads, by predicate or `rdf:type` object.
        let unread: Vec<(u64, &'static str)> = [
            ("FunctionalProperty", "functional property"),
            ("InverseFunctionalProperty", "inverse-functional property"),
            ("ReflexiveProperty", "reflexive property"),
            ("IrreflexiveProperty", "irreflexive property"),
            ("SymmetricProperty", "symmetric property"),
            ("AsymmetricProperty", "asymmetric property"),
            ("propertyDisjointWith", "disjoint properties"),
            ("AllDisjointProperties", "disjoint properties"),
            ("disjointUnionOf", "disjoint union"),
        ]
        .into_iter()
        .map(|(local, kind)| (owl(&mut iri, local), kind))
        .collect();
        let unread = || {
            unread
                .iter()
                .map(|&(term, kind)| (term, Disposition::Unread(kind)))
        };
        names.by_predicate = [
            (names.sub_class_of, Disposition::Class(ClassAxiom::Sub)),
            (
                names.equivalent_class,
                Disposition::Class(ClassAxiom::Equivalent),
            ),
            (
                names.disjoint_with,
                Disposition::Class(ClassAxiom::Disjoint),
            ),
            (
                names.sub_property_of,
                Disposition::Property(PropertyAxiom::Sub { equivalent: false }),
            ),
            (
                names.equivalent_property,
                Disposition::Property(PropertyAxiom::Sub { equivalent: true }),
            ),
            (
                names.property_chain,
                Disposition::Property(PropertyAxiom::Chain),
            ),
            (names.domain, Disposition::Property(PropertyAxiom::Domain)),
            (names.range, Disposition::Property(PropertyAxiom::Range)),
        ]
        .into_iter()
        .chain(unread())
        .chain(
            expression
                .iter()
                .enumerate()
                .map(|(place, &(predicate, part))| {
                    (predicate, Disposition::Expression(place, part))
                }),
        )
        .collect();
        names.by_type = [
            (names.all_disjoint_classes, Disposition::AllDisjointClasses),
            (
                names.transitive,
                Disposition::Property(PropertyAxiom::Transitive),
            ),
        ]
        .into_iter()
        .chain(unread())
        .collect();
        names
    }

    /// Whether the rules can read `p` as a role, `Err` why not: it is an expression (an
    /// inverse), or a top or bottom property.
    fn role(&self, p: u64, named: &dyn Fn(u64) -> bool) -> Result<(), &'static str> {
        if !named(p) {
            Err("inverse property")
        } else if self.top_and_bottom.contains(&p) {
            Err("top or bottom property")
        } else {
            Ok(())
        }
    }

    /// What `s p o` is to [`build`], one lookup; `None` for what states no class or property
    /// axiom: a declaration, an annotation, a list's cell, an assertion about individuals.
    fn disposition(&self, p: u64, o: u64) -> Option<Disposition> {
        match p == self.rdf_type {
            true => self.by_type.get(o),
            false => self.by_predicate.get(p),
        }
    }

    /// What `s p o` declares `s` as, if it is a declaration.
    fn declared(&self, p: u64, o: u64) -> Option<EntityKind> {
        (p == self.rdf_type).then(|| self.declarations.get(o))?
    }

    /// The predicate `p` of the mapping's class expressions and data ranges, if it is one:
    /// its place in their table and what EL makes of it.
    fn expression(&self, p: u64) -> Option<(usize, Part)> {
        match self.by_predicate.get(p) {
            Some(Disposition::Expression(place, part)) => Some((place, part)),
            _ => None,
        }
    }

    /// The property axiom `axiom` of `s p o` where the properties it relates are roles: the
    /// subject, and a sub- or equivalent property's object; `Err` why one isn't.
    fn roles(
        &self,
        axiom: PropertyAxiom,
        [s, _, o]: Triple,
        named: &dyn Fn(u64) -> bool,
    ) -> Result<PropertyAxiom, &'static str> {
        self.role(s, named).and_then(|()| match axiom {
            PropertyAxiom::Sub { .. } => self.role(o, named),
            _ => Ok(()),
        })?;
        Ok(axiom)
    }
}

/// A property axiom the rules read, between roles ([`Names::roles`]).
#[derive(Debug, Clone, Copy)]
enum PropertyAxiom {
    /// `s ⊑ o`, and `o ⊑ s` where they are equivalent.
    Sub {
        equivalent: bool,
    },
    Transitive,
    /// `s` from the chain of the list `o`.
    Chain,
    Domain,
    Range,
}

/// A class expression, as far as EL has it.
enum Expr {
    Named(u64),
    And(Vec<Expr>),
    Some(u64, Box<Expr>),
}

type Concept = u32;
type Role = u32;

#[derive(Default)]
struct Tbox {
    /// Concept index of each named class (and of the fresh ones: `None` there).
    named: Vec<Option<u64>>,
    index: HashMap<u64, Concept>,
    told: Vec<Vec<Concept>>,
    /// `A ⊓ B ⊑ D`, indexed by `A` (and by `B`).
    conjunctions: Vec<Vec<(Concept, Concept)>>,
    /// `A ⊑ ∃r.B`, by `A`.
    existentials: Vec<Vec<(Role, Concept)>>,
    /// `∃r.B ⊑ D`, by `B`.
    filler_of: Vec<Vec<(Role, Concept)>>,
    roles: HashMap<u64, Role>,
    role_count: u32,
    sub_roles: Vec<(Role, Role)>,
    chains: Vec<(Role, Role, Role)>,
    ranges: Vec<(Role, Concept)>,
    /// Interned conjunctions of fresh names (`and(a, b)`), so that equal ones share one.
    ands: HashMap<(Concept, Concept), Concept>,
    somes: HashMap<(Role, Concept), Concept>,
}

impl Tbox {
    fn concept(&mut self, named: Option<u64>) -> Concept {
        if let Some(id) = named
            && let Some(&c) = self.index.get(&id)
        {
            return c;
        }
        let c = self.named.len() as Concept;
        self.named.push(named);
        self.told.push(Vec::new());
        self.conjunctions.push(Vec::new());
        self.existentials.push(Vec::new());
        self.filler_of.push(Vec::new());
        if let Some(id) = named {
            self.index.insert(id, c);
        }
        c
    }

    fn role(&mut self, id: Option<u64>) -> Role {
        if let Some(id) = id
            && let Some(&r) = self.roles.get(&id)
        {
            return r;
        }
        let r = self.role_count;
        self.role_count += 1;
        if let Some(id) = id {
            self.roles.insert(id, r);
        }
        r
    }

    fn sub(&mut self, a: Concept, b: Concept) {
        if a != b {
            self.told[a as usize].push(b);
        }
    }

    fn and_sub(&mut self, a: Concept, b: Concept, d: Concept) {
        self.conjunctions[a as usize].push((b, d));
        self.conjunctions[b as usize].push((a, d));
    }

    /// A concept equivalent to `a ⊓ b`.
    fn and(&mut self, a: Concept, b: Concept) -> Concept {
        if a == b {
            return a;
        }
        let key = (a.min(b), a.max(b));
        if let Some(&c) = self.ands.get(&key) {
            return c;
        }
        let c = self.concept(None);
        self.sub(c, a);
        self.sub(c, b);
        self.and_sub(a, b, c);
        self.ands.insert(key, c);
        c
    }

    /// A concept equivalent to `∃r.b`.
    fn some(&mut self, r: Role, b: Concept) -> Concept {
        if let Some(&c) = self.somes.get(&(r, b)) {
            return c;
        }
        let c = self.concept(None);
        self.existentials[c as usize].push((r, b));
        self.filler_of[b as usize].push((r, c));
        self.somes.insert((r, b), c);
        c
    }

    fn name(&mut self, expr: &Expr) -> Concept {
        match expr {
            Expr::Named(id) => self.concept(Some(*id)),
            Expr::And(parts) => {
                let concepts: Vec<Concept> = parts.iter().map(|p| self.name(p)).collect();
                let (&first, rest) = concepts.split_first().expect("a conjunction has parts");
                rest.iter().fold(first, |acc, &c| self.and(acc, c))
            }
            Expr::Some(role, filler) => {
                let r = self.role(Some(*role));
                let b = self.name(filler);
                self.some(r, b)
            }
        }
    }
}

/// Why an EL RDF list could not be read. A resource stop says nothing about its tail.
#[derive(Debug)]
enum ListError {
    Malformed,
    Cyclic,
    ResourceLimit,
}

impl ListError {
    fn reason(self) -> &'static str {
        match self {
            Self::Malformed => "malformed list",
            Self::Cyclic => "cyclic list",
            Self::ResourceLimit => "list resource limit exceeded",
        }
    }
}

/// Two different objects where the mapping has one subject and predicate say one thing.
struct Repeated;

/// A subject of at most this many pairs keeps them as read, the same triple twice included:
/// scanning so few (two cache lines) is cheaper than sorting them, and most subjects are as
/// small (a restriction, a list's cell).
const SCANNED: usize = 8;

/// The triples of an ontology, indexed by subject and predicate.
struct Graph<'a> {
    /// Each subject's `(predicate, object)` pairs; past [`SCANNED`] sorted and distinct, so
    /// that a lookup is a binary search, not a scan of every triple of the subject (a node
    /// many axioms name has as many).
    by_subject: HashMap<u64, Vec<(u64, u64)>>,
    names: &'a Names,
    /// The declared data properties, whose restrictions no rule reads.
    data_properties: HashSet<u64>,
    /// The declared annotation properties, whose domains, ranges and sub-properties say
    /// nothing about classes.
    annotation_properties: HashSet<u64>,
    /// The blank nodes no class expression of EL can be read from, whatever uses them, and
    /// why: those declared datatypes, a data range, and those whose predicates of the
    /// mapping's class expressions say something EL lacks, decided once per node.
    refused: HashMap<u64, &'static str>,
    /// The named datatypes (OWL 2's datatype map and those declared datatypes,
    /// `rdfs:Datatype` or `owl:DataRange`): a named class read is one of these or itself.
    datatypes: TermTable<()>,
}

impl<'a> Graph<'a> {
    fn new(triples: &[Triple], names: &'a Names, named: &dyn Fn(u64) -> bool) -> Self {
        let mut graph = Graph {
            by_subject: HashMap::new(),
            names,
            data_properties: HashSet::new(),
            annotation_properties: HashSet::new(),
            refused: HashMap::new(),
            datatypes: std::iter::empty().collect(),
        };
        let mut declared = Vec::new();
        for &[s, p, o] in triples {
            graph.by_subject.entry(s).or_default().push((p, o));
            match names.declared(p, o) {
                Some(EntityKind::Datatype) => declared.push(s),
                Some(EntityKind::DataProperty) => graph.data_properties.extend([s]),
                Some(EntityKind::AnnotationProperty) => graph.annotation_properties.extend([s]),
                _ => {}
            }
        }
        for (&subject, pairs) in graph.by_subject.iter_mut() {
            // The same triple twice is one: past `SCANNED` each predicate's range holds
            // distinct objects; below, a scan skips an earlier copy.
            let sorted = pairs.len() > SCANNED;
            if sorted {
                pairs.sort_unstable();
                pairs.dedup();
            }
            // One node, one expression: a field given twice, or two constructors, read or
            // not, say two things; a field EL reads no restriction of, or a constructor EL
            // doesn't read, says one it lacks. Decided here, a lookup a pair, so that a node
            // many axioms name is decided once, and before any of its values is read. A named
            // node is read as itself, so only blank ones are decided.
            if named(subject) {
                continue;
            }
            let (mut constructors, mut repeated, mut previous) = (0, false, None);
            let (mut unreadable, mut unread) = (None, None);
            for (i, &pair) in pairs.iter().enumerate() {
                let (predicate, _) = pair;
                let Some((place, part)) = names.expression(predicate) else {
                    continue;
                };
                if sorted {
                    // Sorted and distinct: the predicate again is a second value.
                    repeated |= previous == Some(predicate);
                    previous = Some(predicate);
                } else {
                    // As read: the same pair earlier is the same triple, the predicate
                    // earlier with another object a second value.
                    let earlier = &pairs[..i];
                    if earlier.contains(&pair) {
                        continue;
                    }
                    repeated |= earlier.iter().any(|&(p, _)| p == predicate);
                }
                match part {
                    Part::Read => constructors += 1,
                    Part::Unread(kind) => {
                        constructors += 1;
                        unread = Some(kind);
                    }
                    // The table's first such field names the omission.
                    Part::Field(Some(kind))
                        if unreadable.is_none_or(|(first, _)| place < first) =>
                    {
                        unreadable = Some((place, kind));
                    }
                    Part::Field(_) => {}
                }
            }
            let refused = if repeated || constructors > 1 {
                Some("malformed class expression")
            } else {
                unreadable.map(|(_, kind)| kind).or(unread)
            };
            if let Some(kind) = refused {
                graph.refused.insert(subject, kind);
            }
        }
        // A datatype read as a class is a data range, whatever else its node says. The
        // datatype map's are IRIs, and `named` is asked of the input's terms only. Each
        // declared datatype is held once, however often its declaration repeats.
        declared.sort_unstable();
        declared.dedup();
        let (named_declared, blank_declared): (Vec<u64>, Vec<u64>) =
            declared.into_iter().partition(|&datatype| named(datatype));
        graph.datatypes = names
            .datatype_map
            .iter()
            .copied()
            .chain(named_declared)
            .map(|datatype| (datatype, ()))
            .collect();
        for datatype in blank_declared {
            graph.refused.insert(datatype, "data range");
        }
        graph
    }
}

impl Graph<'_> {
    /// The object of `subject predicate` where the mapping has at most one; two different
    /// ones are [`Repeated`]. Up to [`SCANNED`] pairs a scan, past it a binary search: the
    /// predicate's objects first, distinct.
    fn one(&self, subject: u64, predicate: u64) -> Result<Option<u64>, Repeated> {
        let pairs = self.by_subject.get(&subject).map_or(&[][..], Vec::as_slice);
        if pairs.len() <= SCANNED {
            let mut objects = pairs
                .iter()
                .filter(|&&(p, _)| p == predicate)
                .map(|&(_, o)| o);
            return match objects.next() {
                None => Ok(None),
                Some(first) if objects.all(|o| o == first) => Ok(Some(first)),
                Some(_) => Err(Repeated),
            };
        }
        match &pairs[pairs.partition_point(|&(p, _)| p < predicate)..] {
            [(p, _), (q, _), ..] if *p == predicate && *q == predicate => Err(Repeated),
            [(p, o), ..] if *p == predicate => Ok(Some(*o)),
            _ => Ok(None),
        }
    }

    fn list(&self, mut node: u64) -> Result<Vec<u64>, ListError> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        while node != self.names.nil {
            if !seen.insert(node) {
                return Err(ListError::Cyclic);
            }
            if out.len() > 10_000 {
                return Err(ListError::ResourceLimit);
            }
            let one = move |predicate| self.one(node, predicate).ok().flatten();
            out.push(one(self.names.first).ok_or(ListError::Malformed)?);
            node = one(self.names.rest).ok_or(ListError::Malformed)?;
        }
        Ok(out)
    }

    /// The class expression `node` is; `Err` names what EL lacks.
    /// Each node read is a unit of `checkpoint`'s; once it has fired, `Err` (the caller's
    /// next tick sees it).
    fn expr(
        &self,
        node: u64,
        named: &dyn Fn(u64) -> bool,
        depth: usize,
        checkpoint: &Checkpoint<'_>,
    ) -> Result<Expr, &'static str> {
        if checkpoint.tick().is_err() {
            return Err("stopped");
        }
        if depth > 64 {
            return Err("too deeply nested");
        }
        if named(node) {
            return match self.datatypes.get(node) {
                Some(()) => Err("data range"),
                None => Ok(Expr::Named(node)),
            };
        }
        // A blank datatype, or a blank node decided over the mapping's whole vocabulary when
        // the graph was built: none of its values is read while it says something else as well.
        if let Some(&kind) = self.refused.get(&node) {
            return Err(kind);
        }
        let one = |predicate| self.one(node, predicate).ok().flatten();
        if let Some(list) = one(self.names.intersection_of) {
            let members = self.list(list).map_err(ListError::reason)?;
            if members.is_empty() {
                return Err("empty intersection");
            }
            return members
                .into_iter()
                .map(|m| self.expr(m, named, depth + 1, checkpoint))
                .collect::<Result<Vec<_>, _>>()
                .map(Expr::And);
        }
        if let (Some(filler), Some(role)) = (
            one(self.names.some_values_from),
            one(self.names.on_property),
        ) {
            self.names.role(role, named)?;
            if self.data_properties.contains(&role) {
                return Err("data restriction");
            }
            return Ok(Expr::Some(
                role,
                Box::new(self.expr(filler, named, depth + 1, checkpoint)?),
            ));
        }
        Err("unknown class expression")
    }
}

/// Classifies the EL part of the ontology in `triples`. `named` tells IRIs (named classes
/// and properties) from blank nodes (class expressions, lists).
pub fn classify(
    triples: &[Triple],
    vocabulary: &mut impl Vocabulary,
    named: &dyn Fn(u64) -> bool,
) -> Classification {
    classify_profiled(triples, vocabulary, named, 1).0
}

/// Like [`classify`], saturating on `threads` workers (ELK's scheme, *The Incredible ELK*,
/// JAR 2014): one context per concept with its own inbox, activated by compare-and-swap,
/// so that a context's state is only ever touched by the one worker that holds its
/// activation. A link `C →r D` is two messages, a backward link to `D` and a forward link
/// to `C`, so that every rule finds its premises in one context. The result is
/// [`classify`]'s: saturation has one fixpoint. One thread runs [`classify`].
pub fn classify_parallel(
    triples: &[Triple],
    vocabulary: &mut impl Vocabulary,
    named: &dyn Fn(u64) -> bool,
    threads: usize,
) -> Classification {
    classify_profiled(triples, vocabulary, named, threads).0
}

/// [`classify_parallel`] with its [`Profile`]: the phases' times and the counters.
pub fn classify_profiled(
    triples: &[Triple],
    vocabulary: &mut impl Vocabulary,
    named: &dyn Fn(u64) -> bool,
    threads: usize,
) -> (Classification, Profile) {
    let no_memory = || -> Option<nrese_exec::memory::MemoryStopCause> { None };
    let never = OperationStop::with_poll(super::eval::NEVER, &no_memory);
    match classify_profiled_within(triples, vocabulary, named, threads, &never) {
        Ok(result) => result,
        Err(_) => unreachable!("a stop that never fires doesn't stop"),
    }
}

/// [`classify_parallel`] under `stop`: once it fires (a deadline, the memory limit), every
/// worker stops within [`nrese_exec::stop::POLL_UNITS`] units of work (a message handled or
/// sent, a class or pair assembled, a step of reading an axiom), the work so far is
/// discarded, and the run answers its [`Interrupted`], never a partial classification. A
/// soft bound: an excess is seen by the poll after it, not refused in advance. Unpolled,
/// each growing with the input or the answer, not with time: indexing the triples, the
/// role closure (`prepare`) and the answer's final sorts.
pub fn classify_parallel_within(
    triples: &[Triple],
    vocabulary: &mut impl Vocabulary,
    named: &dyn Fn(u64) -> bool,
    threads: usize,
    stop: &OperationStop<'_>,
) -> Result<Classification, Interrupted> {
    classify_profiled_within(triples, vocabulary, named, threads, stop).map(|(c, _)| c)
}

/// [`classify_parallel_within`] with its [`Profile`].
pub fn classify_profiled_within(
    triples: &[Triple],
    vocabulary: &mut impl Vocabulary,
    named: &dyn Fn(u64) -> bool,
    threads: usize,
    stop: &OperationStop<'_>,
) -> Result<(Classification, Profile), Interrupted> {
    let fired = || stop.fired();
    let checkpoint = Checkpoint {
        poll: Poller::new(&fired).ok_or_else(|| stop.interrupted())?,
        stop,
    };
    let started = Instant::now();
    let (mut tbox, thing, nothing, skipped) = build(triples, vocabulary, named, &checkpoint)?;
    let mut profile = Profile {
        normalise: started.elapsed(),
        concepts: tbox.named.len(),
        threads: threads.max(1),
        ..Profile::default()
    };
    let result = if threads <= 1 {
        saturate(
            &mut tbox,
            thing,
            nothing,
            skipped,
            &mut profile,
            &checkpoint,
        )
    } else {
        parallel::saturate(
            &mut tbox,
            thing,
            nothing,
            skipped,
            threads,
            &mut profile,
            &checkpoint,
        )
    }?;
    Ok((result, profile))
}

type Built = (Tbox, Concept, Concept, Vec<(u64, &'static str)>);

/// The normalised TBox of `triples`, with `owl:Thing`, `owl:Nothing` and what was skipped;
/// counting per triple, per class expression node read, per disjoint pair and per
/// expansion step.
fn build(
    triples: &[Triple],
    vocabulary: &mut impl Vocabulary,
    named: &dyn Fn(u64) -> bool,
    checkpoint: &Checkpoint<'_>,
) -> Result<Built, Interrupted> {
    let names = Names::new(vocabulary);
    let graph = Graph::new(triples, &names, named);
    let mut tbox = Tbox::default();
    let thing = tbox.concept(Some(names.thing));
    let nothing = tbox.concept(Some(names.nothing));
    let mut skipped = Vec::new();
    let expr = |node: u64, predicate: u64, skipped: &mut Vec<(u64, &'static str)>| match graph
        .expr(node, named, 0, checkpoint)
    {
        Ok(e) => Some(e),
        Err(kind) => {
            skipped.push((predicate, kind));
            None
        }
    };
    // Once the checkpoint has fired (an expression's read reports it as a skip), every
    // tick answers the stop: the run stops, what was read discarded.
    // Every declared class is classified, also one no axiom names.
    for &[s, p, o] in triples {
        checkpoint.tick()?;
        if named(s) && names.declared(p, o) == Some(EntityKind::Class) {
            let _ = tbox.concept(Some(s));
        }
    }
    for &[s, p, o] in triples {
        checkpoint.tick()?;
        let Some(disposition) = names.disposition(p, o) else {
            continue;
        };
        match disposition {
            Disposition::Class(axiom) => {
                let (Some(a), Some(b)) = (expr(s, p, &mut skipped), expr(o, p, &mut skipped))
                else {
                    continue;
                };
                let (a, b) = (tbox.name(&a), tbox.name(&b));
                match axiom {
                    ClassAxiom::Sub => tbox.sub(a, b),
                    ClassAxiom::Equivalent => {
                        tbox.sub(a, b);
                        tbox.sub(b, a);
                    }
                    ClassAxiom::Disjoint => tbox.and_sub(a, b, nothing),
                }
            }
            Disposition::AllDisjointClasses => {
                let members = match graph
                    .one(s, names.members)
                    .ok()
                    .flatten()
                    .ok_or(ListError::Malformed)
                    .and_then(|l| graph.list(l))
                {
                    Ok(members) => members,
                    Err(error) => {
                        skipped.push((p, error.reason()));
                        continue;
                    }
                };
                let concepts: Vec<Concept> = members
                    .iter()
                    .filter_map(|&m| expr(m, p, &mut skipped))
                    .map(|e| tbox.name(&e))
                    .collect();
                for (i, &a) in concepts.iter().enumerate() {
                    for &b in &concepts[i + 1..] {
                        checkpoint.tick()?;
                        #[cfg(test)]
                        tests::unit(checkpoint.stop);
                        tbox.and_sub(a, b, nothing);
                    }
                }
            }
            // An annotation property's domain, range and sub-properties say nothing about
            // classes, as nrese-owl's mapping reads them.
            Disposition::Property(
                PropertyAxiom::Sub { equivalent: false }
                | PropertyAxiom::Domain
                | PropertyAxiom::Range,
            ) if graph.annotation_properties.contains(&s) => {}
            Disposition::Property(axiom) => match names.roles(axiom, [s, p, o], named) {
                Err(kind) => skipped.push((p, kind)),
                Ok(PropertyAxiom::Sub { equivalent }) => {
                    let (r, t) = (tbox.role(Some(s)), tbox.role(Some(o)));
                    tbox.sub_roles.push((r, t));
                    if equivalent {
                        tbox.sub_roles.push((t, r));
                    }
                }
                Ok(PropertyAxiom::Transitive) => {
                    let r = tbox.role(Some(s));
                    tbox.chains.push((r, r, r));
                }
                Ok(PropertyAxiom::Chain) => {
                    let links = match graph.list(o) {
                        Ok(links) => links,
                        Err(error) => {
                            skipped.push((p, error.reason()));
                            continue;
                        }
                    };
                    if links.len() < 2 {
                        skipped.push((p, "chain of fewer than two properties"));
                        continue;
                    }
                    if let Some(kind) = links.iter().find_map(|&l| names.role(l, named).err()) {
                        skipped.push((p, kind));
                        continue;
                    }
                    let target = tbox.role(Some(s));
                    let mut roles: Vec<Role> = links.iter().map(|&l| tbox.role(Some(l))).collect();
                    // r1 ∘ r2 ∘ … ∘ rn ⊑ s as (((r1 ∘ r2) ∘ r3) …) with fresh roles.
                    while roles.len() > 2 {
                        checkpoint.tick()?;
                        let fresh = tbox.role(None);
                        tbox.chains.push((roles[0], roles[1], fresh));
                        roles.splice(0..2, [fresh]);
                    }
                    tbox.chains.push((roles[0], roles[1], target));
                }
                // A data property's domain and range say nothing about classes but through a
                // restriction on the property, which is reported (the top and bottom data
                // properties, which do, aren't roles).
                Ok(PropertyAxiom::Domain | PropertyAxiom::Range)
                    if graph.data_properties.contains(&s) => {}
                Ok(PropertyAxiom::Domain) => {
                    let Some(c) = expr(o, p, &mut skipped) else {
                        continue;
                    };
                    let (r, c) = (tbox.role(Some(s)), tbox.name(&c));
                    let some = tbox.some(r, thing);
                    tbox.sub(some, c);
                }
                // A datatype as an object property's range is a datatype read as a class:
                // `expr` reports it.
                Ok(PropertyAxiom::Range) => {
                    let Some(c) = expr(o, p, &mut skipped) else {
                        continue;
                    };
                    let (r, c) = (tbox.role(Some(s)), tbox.name(&c));
                    tbox.ranges.push((r, c));
                }
            },
            Disposition::Unread(kind) => skipped.push((p, kind)),
            // An expression's triples hang off its blank node and are read through the axiom
            // that uses it; on a named subject they define that class (or, `owl:inverseOf`,
            // relate two properties).
            Disposition::Expression(_, part) if named(s) => skipped.push((
                p,
                match part {
                    Part::Unread(kind) | Part::Field(Some(kind)) => kind,
                    Part::Read | Part::Field(None) => "named class expression",
                },
            )),
            Disposition::Expression(..) => {}
        }
    }
    checkpoint.finish()?;
    Ok((tbox, thing, nothing, skipped))
}

/// The role closure and chain indexes, and the ranges folded into the existentials: what
/// both saturations need before the rules run.
struct Prepared {
    /// Every role's super-roles (reflexive, transitive).
    supers: Vec<Vec<Role>>,
    /// `r1 ∘ r2 ⊑ t` by `r1`: `(r2, t)`.
    chains_first: Vec<Vec<(Role, Role)>>,
    /// ... and by `r2`: `(r1, t)`.
    chains_second: Vec<Vec<(Role, Role)>>,
}

fn prepare(tbox: &mut Tbox) -> Prepared {
    let roles = tbox.role_count as usize;
    let mut supers: Vec<Vec<Role>> = (0..roles as Role).map(|r| vec![r]).collect();
    loop {
        let mut changed = false;
        for &(r, s) in &tbox.sub_roles {
            let add: Vec<Role> = supers[s as usize].clone();
            for t in add {
                if !supers[r as usize].contains(&t) {
                    supers[r as usize].push(t);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Ranges apply to every edge of a sub-role: `A ⊑ ∃r.B` becomes `A ⊑ ∃r.(B ⊓ R)`.
    if !tbox.ranges.is_empty() {
        let ranges = tbox.ranges.clone();
        for c in 0..tbox.named.len() {
            let edges = std::mem::take(&mut tbox.existentials[c]);
            let mut updated = Vec::with_capacity(edges.len());
            for (r, b) in edges {
                let mut target = b;
                for &(ranged, class) in &ranges {
                    if supers[r as usize].contains(&ranged) {
                        target = tbox.and(target, class);
                    }
                }
                updated.push((r, target));
            }
            tbox.existentials[c] = updated;
        }
    }
    let mut chains_first: Vec<Vec<(Role, Role)>> = vec![Vec::new(); roles];
    let mut chains_second: Vec<Vec<(Role, Role)>> = vec![Vec::new(); roles];
    for &(r1, r2, t) in &tbox.chains {
        chains_first[r1 as usize].push((r2, t));
        chains_second[r2 as usize].push((r1, t));
    }
    Prepared {
        supers,
        chains_first,
        chains_second,
    }
}

/// The classification from every concept's subsumers, counting per class and pair.
fn assemble(
    tbox: &Tbox,
    subsumers: &[HashSet<Concept>],
    thing: Concept,
    nothing: Concept,
    skipped: Vec<(u64, &'static str)>,
    checkpoint: &Checkpoint<'_>,
) -> Result<Classification, Interrupted> {
    // A class or a pair looked at.
    let unit = || -> Result<(), Interrupted> {
        checkpoint.tick()?;
        #[cfg(test)]
        tests::unit(checkpoint.stop);
        Ok(())
    };
    let mut result = Classification {
        skipped,
        ..Classification::default()
    };
    let named_ids: Vec<(Concept, u64)> = tbox
        .named
        .iter()
        .enumerate()
        .filter_map(|(c, id)| id.map(|id| (c as Concept, id)))
        .collect();
    let top = &subsumers[thing as usize];
    for &(d, id) in &named_ids {
        unit()?;
        if d != thing && d != nothing && top.contains(&d) {
            result.top.push(id);
        }
    }
    for &(c, id) in &named_ids {
        if c == thing || c == nothing {
            continue;
        }
        let set = &subsumers[c as usize];
        unit()?;
        if set.contains(&nothing) {
            result.unsatisfiable.push(id);
            continue;
        }
        // Its own subsumers, not every named class: linear in the taxonomy's size.
        for &d in set {
            unit()?;
            if d != c
                && d != thing
                && let Some(super_id) = tbox.named[d as usize]
            {
                result.subsumptions.push((id, super_id));
            }
        }
    }
    checkpoint.finish()?;
    result.subsumptions.sort_unstable();
    result.unsatisfiable.sort_unstable();
    result.top.sort_unstable();
    Ok(result)
}

mod parallel {
    //! The parallel saturation of [`super::classify_parallel`].

    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::{Mutex, MutexGuard};

    use hashbrown::HashSet;
    use nrese_exec::stop::POLL_UNITS;

    use std::time::Instant;

    use super::{
        Checkpoint, Classification, Concept, Interrupted, OperationStop, Prepared, Profile, Role,
        Tbox, assemble, prepare,
    };

    thread_local! {
        /// This worker's units left before its next poll: counted per thread, since an
        /// activation is too short to reach [`POLL_UNITS`] alone.
        static LEFT: Cell<u32> = const { Cell::new(POLL_UNITS) };
    }

    enum Message {
        /// A subsumer of the context's concept.
        Sub(Concept),
        /// `from →r` this context.
        Backward(Concept, Role),
        /// This context `→r to`.
        Forward(Role, Concept),
    }

    #[derive(Default)]
    struct State {
        subsumers: HashSet<Concept>,
        pred: Vec<(Concept, Role)>,
        pred_seen: HashSet<(Concept, Role)>,
        succ: Vec<(Role, Concept)>,
        succ_seen: HashSet<(Role, Concept)>,
        /// Messages handled, and how many repeated what the context knew ([`Profile`]).
        handled: u64,
        repeated: u64,
    }

    #[derive(Default)]
    struct Context {
        inbox: Mutex<Vec<Message>>,
        active: AtomicBool,
        initialised: AtomicBool,
        /// Only the worker holding the activation locks it: never contended.
        state: Mutex<State>,
    }

    /// A poisoned lock means a worker panicked; the scope re-raises that panic anyway.
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct Saturation<'a> {
        tbox: &'a Tbox,
        prepared: Prepared,
        contexts: Vec<Context>,
        thing: Concept,
        nothing: Concept,
        /// The run's one stop: polled every [`POLL_UNITS`] units per worker; once it has
        /// fired (its latch), every worker drops what it holds and sends nothing more.
        stop: &'a OperationStop<'a>,
    }

    pub(super) fn saturate(
        tbox: &mut Tbox,
        thing: Concept,
        nothing: Concept,
        skipped: Vec<(u64, &'static str)>,
        threads: usize,
        profile: &mut Profile,
        checkpoint: &Checkpoint<'_>,
    ) -> Result<Classification, Interrupted> {
        let started = Instant::now();
        let prepared = prepare(tbox);
        profile.prepare = started.elapsed();
        let started = Instant::now();
        let n = tbox.named.len();
        let saturation = Saturation {
            tbox,
            prepared,
            contexts: (0..n).map(|_| Context::default()).collect(),
            thing,
            nothing,
            stop: checkpoint.stop,
        };
        let run = || {
            rayon::scope(|scope| {
                // A concept looked at and a message delivered are units; stopped, no more.
                let _ = (0..n as Concept).try_for_each(|c| {
                    saturation.tick()?;
                    if saturation.tbox.named[c as usize].is_some()
                        && !saturation.contexts[c as usize]
                            .initialised
                            .swap(true, SeqCst)
                    {
                        for message in [Message::Sub(c), Message::Sub(thing)] {
                            saturation.tick()?;
                            #[cfg(test)]
                            super::tests::unit(saturation.stop);
                            saturation.deliver(scope, c, message);
                        }
                    }
                    Ok::<_, Interrupted>(())
                });
            });
        };
        match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
            Ok(pool) => pool.install(run),
            Err(_) => run(),
        }
        let initialised = saturation
            .contexts
            .iter()
            .filter(|c| c.initialised.load(SeqCst))
            .count();
        let subsumers: Vec<HashSet<Concept>> = saturation
            .contexts
            .into_iter()
            .map(|c| {
                let state = c.state.into_inner().unwrap_or_else(|p| p.into_inner());
                profile.links += state.succ.len();
                profile.conclusions += state.handled;
                profile.duplicates += state.repeated;
                state.subsumers
            })
            .collect();
        profile.saturate = started.elapsed();
        profile.contexts = initialised;
        profile.subsumers = subsumers.iter().map(HashSet::len).sum();
        // Every worker has joined: a stop that fired left the saturation partial, and
        // nothing of it is assembled.
        if checkpoint.stop.latched() {
            return Err(checkpoint.stop.interrupted());
        }
        let started = Instant::now();
        let result = assemble(tbox, &subsumers, thing, nothing, skipped, checkpoint)?;
        profile.assemble = started.elapsed();
        Ok(result)
    }

    /// The context a worker is saturating, and the messages it sends itself: those are
    /// handled in the same activation, without the inbox.
    struct Own<'o> {
        context: Concept,
        local: &'o mut Vec<Message>,
    }

    impl Saturation<'_> {
        /// One unit of this worker's work; polls the stop when its allowance runs out (which
        /// latches it once it fires). `Err` once the stop has latched, on any worker: the
        /// activation ends, and nothing more is sent.
        fn tick(&self) -> Result<(), Interrupted> {
            LEFT.with(|left| match left.get() {
                1 => {
                    left.set(POLL_UNITS);
                    self.stop.fired();
                }
                rest => left.set(rest - 1),
            });
            match self.stop.latched() {
                true => Err(self.stop.interrupted()),
                false => Ok(()),
            }
        }

        /// A message into `to`'s inbox; `to` gets a worker unless it has one.
        fn deliver<'s>(&'s self, scope: &rayon::Scope<'s>, to: Concept, message: Message) {
            let context = &self.contexts[to as usize];
            lock(&context.inbox).push(message);
            if !context.active.swap(true, SeqCst) {
                scope.spawn(move |scope| self.work(scope, to));
            }
        }

        fn send<'s>(
            &'s self,
            scope: &rayon::Scope<'s>,
            own: &mut Own<'_>,
            to: Concept,
            message: Message,
        ) -> Result<(), Interrupted> {
            self.tick()?;
            #[cfg(test)]
            super::tests::unit(self.stop);
            if to == own.context {
                own.local.push(message);
            } else {
                self.deliver(scope, to, message);
            }
            Ok(())
        }

        /// `from →t to`: a backward link to `to`, a forward link to `from`; `to` starts
        /// saturating if it hasn't.
        fn link<'s>(
            &'s self,
            scope: &rayon::Scope<'s>,
            own: &mut Own<'_>,
            from: Concept,
            t: Role,
            to: Concept,
        ) -> Result<(), Interrupted> {
            if !self.contexts[to as usize].initialised.swap(true, SeqCst) {
                self.send(scope, own, to, Message::Sub(to))?;
                self.send(scope, own, to, Message::Sub(self.thing))?;
            }
            self.send(scope, own, to, Message::Backward(from, t))?;
            self.send(scope, own, from, Message::Forward(t, to))
        }

        /// Processes a context's messages until its inbox stays empty.
        fn work<'s>(&'s self, scope: &rayon::Scope<'s>, c: Concept) {
            let context = &self.contexts[c as usize];
            loop {
                let mut batch = std::mem::take(&mut *lock(&context.inbox));
                if batch.is_empty() {
                    context.active.store(false, SeqCst);
                    // A message delivered between the take and the store found the context
                    // still active and started no worker: take it up here.
                    if lock(&context.inbox).is_empty() || context.active.swap(true, SeqCst) {
                        return;
                    }
                    continue;
                }
                let mut state = lock(&context.state);
                while let Some(message) = batch.pop() {
                    let mut own = Own {
                        context: c,
                        local: &mut batch,
                    };
                    // Stopped, here or on another worker: what this context holds is dropped;
                    // its inbox empties the same way, and the worker returns.
                    if self
                        .tick()
                        .and_then(|()| self.handle(scope, &mut own, &mut state, message))
                        .is_err()
                    {
                        batch.clear();
                        break;
                    }
                }
            }
        }

        fn handle<'s>(
            &'s self,
            scope: &rayon::Scope<'s>,
            own: &mut Own<'_>,
            state: &mut State,
            message: Message,
        ) -> Result<(), Interrupted> {
            #[cfg(test)]
            super::tests::unit(self.stop);
            let (tbox, p, c) = (self.tbox, &self.prepared, own.context);
            state.handled += 1;
            match message {
                Message::Sub(d) => {
                    if !state.subsumers.insert(d) {
                        state.repeated += 1;
                        return Ok(());
                    }
                    for &e in &tbox.told[d as usize] {
                        self.send(scope, own, c, Message::Sub(e))?;
                    }
                    for &(b, e) in &tbox.conjunctions[d as usize] {
                        if state.subsumers.contains(&b) {
                            self.send(scope, own, c, Message::Sub(e))?;
                        }
                    }
                    for &(r, b) in &tbox.existentials[d as usize] {
                        self.link(scope, own, c, r, b)?;
                    }
                    for &(from, r) in &state.pred {
                        if d == self.nothing {
                            self.send(scope, own, from, Message::Sub(self.nothing))?;
                        }
                        for &(s, f) in &tbox.filler_of[d as usize] {
                            if p.supers[r as usize].contains(&s) {
                                self.send(scope, own, from, Message::Sub(f))?;
                            }
                        }
                    }
                }
                Message::Backward(from, r) => {
                    if !state.pred_seen.insert((from, r)) {
                        state.repeated += 1;
                        return Ok(());
                    }
                    state.pred.push((from, r));
                    for &e in &state.subsumers {
                        if e == self.nothing {
                            self.send(scope, own, from, Message::Sub(self.nothing))?;
                        }
                        for &(s, f) in &tbox.filler_of[e as usize] {
                            if p.supers[r as usize].contains(&s) {
                                self.send(scope, own, from, Message::Sub(f))?;
                            }
                        }
                    }
                    // Chains with this link first: from →r c →r2 e.
                    for &s1 in &p.supers[r as usize] {
                        for &(r2, t) in &p.chains_first[s1 as usize] {
                            for &(r2_edge, e) in &state.succ {
                                if p.supers[r2_edge as usize].contains(&r2) {
                                    self.link(scope, own, from, t, e)?;
                                }
                            }
                        }
                    }
                }
                Message::Forward(r, to) => {
                    if !state.succ_seen.insert((r, to)) {
                        state.repeated += 1;
                        return Ok(());
                    }
                    state.succ.push((r, to));
                    // ... and second: b →r1 c →r to.
                    for &s2 in &p.supers[r as usize] {
                        for &(r1, t) in &p.chains_second[s2 as usize] {
                            for &(b, r1_edge) in &state.pred {
                                if p.supers[r1_edge as usize].contains(&r1) {
                                    self.link(scope, own, b, t, to)?;
                                }
                            }
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

fn saturate(
    tbox: &mut Tbox,
    thing: Concept,
    nothing: Concept,
    skipped: Vec<(u64, &'static str)>,
    profile: &mut Profile,
    checkpoint: &Checkpoint<'_>,
) -> Result<Classification, Interrupted> {
    let started = Instant::now();
    let Prepared {
        supers,
        chains_first,
        chains_second,
    } = prepare(tbox);
    profile.prepare = started.elapsed();
    let started = Instant::now();
    let (mut conclusions, mut duplicates) = (0u64, 0u64);
    let n = tbox.named.len();
    let mut subsumers: Vec<HashSet<Concept>> = vec![HashSet::new(); n];
    let mut succ: Vec<Vec<(Role, Concept)>> = vec![Vec::new(); n];
    let mut pred: Vec<Vec<(Concept, Role)>> = vec![Vec::new(); n];
    let mut links: HashSet<(Concept, Role, Concept)> = HashSet::new();
    let mut initialised = vec![false; n];
    enum Item {
        Sub(Concept, Concept),
        Link(Concept, Role, Concept),
    }
    let mut todo: Vec<Item> = Vec::new();
    // A message queued is a unit, as one handled is.
    let queue = |todo: &mut Vec<Item>, item: Item| -> Result<(), Interrupted> {
        checkpoint.tick()?;
        #[cfg(test)]
        tests::unit(checkpoint.stop);
        todo.push(item);
        Ok(())
    };
    let init = |c: Concept,
                initialised: &mut Vec<bool>,
                todo: &mut Vec<Item>|
     -> Result<(), Interrupted> {
        if initialised[c as usize] {
            return Ok(());
        }
        initialised[c as usize] = true;
        queue(todo, Item::Sub(c, c))?;
        queue(todo, Item::Sub(c, thing))
    };
    for c in 0..n as Concept {
        if tbox.named[c as usize].is_some() {
            init(c, &mut initialised, &mut todo)?;
        }
    }
    while let Some(item) = todo.pop() {
        checkpoint.tick()?;
        #[cfg(test)]
        tests::unit(checkpoint.stop);
        conclusions += 1;
        match item {
            Item::Sub(c, d) => {
                if !subsumers[c as usize].insert(d) {
                    duplicates += 1;
                    continue;
                }
                for &e in &tbox.told[d as usize] {
                    queue(&mut todo, Item::Sub(c, e))?;
                }
                for &(b, e) in &tbox.conjunctions[d as usize] {
                    if subsumers[c as usize].contains(&b) {
                        queue(&mut todo, Item::Sub(c, e))?;
                    }
                }
                for &(r, b) in &tbox.existentials[d as usize] {
                    queue(&mut todo, Item::Link(c, r, b))?;
                }
                for &(from, r) in &pred[c as usize] {
                    if d == nothing {
                        queue(&mut todo, Item::Sub(from, nothing))?;
                    }
                    for &(s, f) in &tbox.filler_of[d as usize] {
                        if supers[r as usize].contains(&s) {
                            queue(&mut todo, Item::Sub(from, f))?;
                        }
                    }
                }
            }
            Item::Link(c, r, d) => {
                if !links.insert((c, r, d)) {
                    duplicates += 1;
                    continue;
                }
                init(d, &mut initialised, &mut todo)?;
                succ[c as usize].push((r, d));
                pred[d as usize].push((c, r));
                for &e in &subsumers[d as usize] {
                    if e == nothing {
                        queue(&mut todo, Item::Sub(c, nothing))?;
                    }
                    for &(s, f) in &tbox.filler_of[e as usize] {
                        if supers[r as usize].contains(&s) {
                            queue(&mut todo, Item::Sub(c, f))?;
                        }
                    }
                }
                // Chains with this edge first: c →r d →r2 e.
                for &s1 in &supers[r as usize] {
                    for &(r2, t) in &chains_first[s1 as usize] {
                        for &(r2_edge, e) in &succ[d as usize] {
                            if supers[r2_edge as usize].contains(&r2) {
                                queue(&mut todo, Item::Link(c, t, e))?;
                            }
                        }
                    }
                }
                // ... and second: b →r1 c →r d.
                for &s2 in &supers[r as usize] {
                    for &(r1, t) in &chains_second[s2 as usize] {
                        for &(b, r1_edge) in &pred[c as usize] {
                            if supers[r1_edge as usize].contains(&r1) {
                                queue(&mut todo, Item::Link(b, t, d))?;
                            }
                        }
                    }
                }
            }
        }
    }
    profile.saturate = started.elapsed();
    profile.contexts = initialised.iter().filter(|&&i| i).count();
    profile.subsumers = subsumers.iter().map(HashSet::len).sum();
    profile.links = links.len();
    profile.conclusions = conclusions;
    profile.duplicates = duplicates;
    let started = Instant::now();
    let result = assemble(tbox, &subsumers, thing, nothing, skipped, checkpoint)?;
    profile.assemble = started.elapsed();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::XSD;
    use crate::vocabulary::LocalVocabulary;

    const EX: &str = "http://example.com/";

    /// Loads `s p o` lines of prefixed names (`ex:`, `rdf:`, `rdfs:`, `owl:`, `xsd:`, `_:b`);
    /// `"…"^^<…>` literals (without spaces) are taken as they are.
    fn load(v: &mut LocalVocabulary, text: &str) -> Vec<Triple> {
        let mut term = |t: &str| -> u64 {
            if t.starts_with("_:") || t.starts_with('"') {
                return v.term(t);
            }
            let (prefix, local) = t.split_once(':').unwrap();
            let ns = match prefix {
                "ex" => EX,
                "rdf" => RDF,
                "rdfs" => RDFS,
                "owl" => OWL,
                "xsd" => XSD,
                other => panic!("{other}"),
            };
            v.iri(&format!("{ns}{local}"))
        };
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| {
                let parts: Vec<&str> = l.split_whitespace().collect();
                [term(parts[0]), term(parts[1]), term(parts[2])]
            })
            .collect()
    }

    fn run(text: &str) -> (LocalVocabulary, Classification) {
        let mut v = LocalVocabulary::default();
        let triples = load(&mut v, text);
        let snapshot = v.clone();
        let named = move |id: u64| snapshot.text(id).starts_with('<');
        let result = classify(&triples, &mut v, &named);
        (v, result)
    }

    fn holds(v: &mut LocalVocabulary, result: &Classification, sub: &str, sup: &str) -> bool {
        let (a, b) = (v.iri(&format!("{EX}{sub}")), v.iri(&format!("{EX}{sup}")));
        result.subsumptions.binary_search(&(a, b)).is_ok()
    }

    /// `n` classes, each below the next: n(n+1)/2 subsumers to derive, many polls' work.
    fn chain(n: usize) -> String {
        (0..n)
            .map(|i| format!("ex:C{i} rdfs:subClassOf ex:C{}\n", i + 1))
            .collect()
    }

    /// `text` classified on `threads` workers under a stop whose caller fires from its
    /// `caller_at`-th poll on and whose memory poll from its `memory_at`-th (`0`: never).
    fn within(
        text: &str,
        threads: usize,
        caller_at: u64,
        memory_at: u64,
    ) -> Result<Classification, Interrupted> {
        use nrese_exec::memory::MemoryStopCause;
        use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
        let mut v = LocalVocabulary::default();
        let triples = load(&mut v, text);
        let snapshot = v.clone();
        let named = move |id: u64| snapshot.text(id).starts_with('<');
        let (callers, memories) = (AtomicU64::new(0), AtomicU64::new(0));
        let reached = |count: &AtomicU64, at: u64| at != 0 && count.fetch_add(1, Relaxed) + 1 >= at;
        let caller = || reached(&callers, caller_at);
        let memory = || reached(&memories, memory_at).then_some(MemoryStopCause::OverLimit);
        let stop = OperationStop::with_poll(&caller, &memory);
        classify_parallel_within(&triples, &mut v, &named, threads, &stop)
    }

    /// The bounded entry under a stop that never fires answers what the unbounded one does,
    /// on one worker and on several.
    #[test]
    fn a_stop_that_never_fires_changes_no_classification() {
        let text = chain(300);
        for threads in [1, 4] {
            let mut v = LocalVocabulary::default();
            let triples = load(&mut v, &text);
            let snapshot = v.clone();
            let named = move |id: u64| snapshot.text(id).starts_with('<');
            let unbounded = classify_parallel(&triples, &mut v, &named, threads);
            assert_eq!(unbounded.subsumptions.len(), 300 * 301 / 2);
            assert_eq!(
                within(&text, threads, 0, 0),
                Ok(unbounded),
                "{threads} workers"
            );
        }
    }

    /// A stop that fires during the run ends it with the stop, never a classification: the
    /// work so far is discarded (on one worker and on several), and a memory limit that
    /// fires it is named as its cause.
    #[test]
    fn a_stop_that_fires_during_the_run_answers_no_classification() {
        use nrese_exec::memory::MemoryStopCause;
        let text = chain(300);
        for threads in [1, 4] {
            assert_eq!(
                within(&text, threads, 3, 0),
                Err(Interrupted { memory: None }),
                "{threads} workers"
            );
            assert_eq!(
                within(&text, threads, 0, 3),
                Err(Interrupted {
                    memory: Some(MemoryStopCause::OverLimit)
                }),
                "{threads} workers"
            );
        }
    }

    /// `n` classes in one `owl:AllDisjointClasses` axiom: a first row of n − 1 pairs.
    fn disjoint_row(n: usize) -> String {
        let mut text = String::from("_:d rdf:type owl:AllDisjointClasses\n_:d owl:members _:l0\n");
        for i in 0..n {
            let rest = match i + 1 < n {
                true => format!("_:l{}", i + 1),
                false => "rdf:nil".to_owned(),
            };
            text += &format!("_:l{i} rdf:first ex:C{i}\n_:l{i} rdf:rest {rest}\n");
        }
        text
    }

    /// One class below `n` others: one message's fan-out of n.
    fn fan_out(n: usize) -> String {
        (0..n)
            .map(|i| format!("ex:A rdfs:subClassOf ex:B{i}\n"))
            .collect()
    }

    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};

    /// The run a bound test probes ([`probe`]): its stop's address (0: none), the units of work
    /// it has done ([`unit`]), the units after which its stop turns true, the run's number, and
    /// the most units one thread did after that.
    static PROBED: AtomicUsize = AtomicUsize::new(0);
    static DONE: AtomicU64 = AtomicU64::new(0);
    static AT: AtomicU64 = AtomicU64::new(0);
    static RUN: AtomicU64 = AtomicU64::new(0);
    static MOST: AtomicU64 = AtomicU64::new(0);

    thread_local! {
        /// This thread's units after the probed stop turned true: (run, units).
        static AFTER: std::cell::Cell<(u64, u64)> = const { std::cell::Cell::new((0, 0)) };
    }

    /// A unit of work done under `stop`, counted where it is done (a message handled or
    /// sent, a disjoint pair made, a class or pair assembled) if `stop` is the probed run's.
    pub(super) fn unit(stop: &OperationStop<'_>) {
        if PROBED.load(SeqCst) != std::ptr::from_ref(stop) as usize
            || DONE.fetch_add(1, SeqCst) < AT.load(SeqCst)
        {
            return;
        }
        let run = RUN.load(SeqCst);
        let after = AFTER.with(|after| {
            let units = match after.get() {
                (r, units) if r == run => units + 1,
                _ => 1,
            };
            after.set((run, units));
            units
        });
        MOST.fetch_max(after, SeqCst);
    }

    /// `text` classified on `threads` workers under a stop that turns true once the run has
    /// done `at` units of work: the answer, the most units one worker did after that, and the
    /// units the run did. One probed run at a time.
    fn probe(
        text: &str,
        threads: usize,
        at: u64,
    ) -> (Result<Classification, Interrupted>, u64, u64) {
        static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _one = ONE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut v = LocalVocabulary::default();
        let triples = load(&mut v, text);
        let snapshot = v.clone();
        let named = move |id: u64| snapshot.text(id).starts_with('<');
        let caller = || DONE.load(SeqCst) >= AT.load(SeqCst);
        let no_memory = || -> Option<nrese_exec::memory::MemoryStopCause> { None };
        let stop = OperationStop::with_poll(&caller, &no_memory);
        DONE.store(0, SeqCst);
        AT.store(at, SeqCst);
        MOST.store(0, SeqCst);
        RUN.fetch_add(1, SeqCst);
        PROBED.store(std::ptr::from_ref(&stop) as usize, SeqCst);
        let answer = classify_parallel_within(&triples, &mut v, &named, threads, &stop);
        PROBED.store(0, SeqCst);
        (answer, MOST.load(SeqCst), DONE.load(SeqCst))
    }

    /// Once its stop turns true, a run does at most `POLL_UNITS` more units of work, also
    /// where one step holds more: a disjointness row of 4999 pairs, its stop turning true
    /// at the first.
    #[test]
    fn a_disjointness_row_longer_than_a_poll_stops_within_one() {
        let (answer, most, _) = probe(&disjoint_row(5000), 1, 1);
        assert_eq!(answer, Err(Interrupted { memory: None }));
        let poll = u64::from(nrese_exec::stop::POLL_UNITS);
        assert!(most <= poll, "{most} units after the stop");
    }

    /// Once its stop turns true, each worker does at most `POLL_UNITS` more units of work,
    /// also where one message fans out to more (10,000 subsumers of one class), wherever in
    /// the run the stop turns true (every 2048th unit), on one worker and on four.
    #[test]
    fn a_fan_out_longer_than_a_poll_stops_within_one_on_every_worker() {
        let text = fan_out(10_000);
        let poll = u64::from(nrese_exec::stop::POLL_UNITS);
        for threads in [1, 4] {
            let (whole, _, done) = probe(&text, threads, u64::MAX);
            assert!(whole.is_ok(), "{threads} workers");
            for at in (1..done).step_by(2048) {
                let (_, most, _) = probe(&text, threads, at);
                assert!(
                    most <= poll,
                    "{threads} workers, stop at unit {at}: {most} units after it"
                );
            }
        }
    }

    /// The textbook example (Baader et al.): pericarditis is a heart disease, through an
    /// existential, a conjunction and a property chain.
    #[test]
    fn pericarditis_is_a_heart_disease() {
        let (mut v, result) = run("ex:Pericardium rdfs:subClassOf _:i1
             _:i1 owl:intersectionOf _:l1
             _:l1 rdf:first ex:Tissue
             _:l1 rdf:rest _:l2
             _:l2 rdf:first _:r1
             _:l2 rdf:rest rdf:nil
             _:r1 owl:onProperty ex:partOf
             _:r1 owl:someValuesFrom ex:Heart
             ex:Pericarditis owl:equivalentClass _:i2
             _:i2 owl:intersectionOf _:m1
             _:m1 rdf:first ex:Inflammation
             _:m1 rdf:rest _:m2
             _:m2 rdf:first _:r2
             _:m2 rdf:rest rdf:nil
             _:r2 owl:onProperty ex:hasLocation
             _:r2 owl:someValuesFrom ex:Pericardium
             ex:Inflammation rdfs:subClassOf ex:Disease
             ex:HeartDisease owl:equivalentClass _:i3
             _:i3 owl:intersectionOf _:n1
             _:n1 rdf:first ex:Disease
             _:n1 rdf:rest _:n2
             _:n2 rdf:first _:r3
             _:n2 rdf:rest rdf:nil
             _:r3 owl:onProperty ex:hasLocation
             _:r3 owl:someValuesFrom ex:Heart
             ex:hasLocation owl:propertyChainAxiom _:c1
             _:c1 rdf:first ex:hasLocation
             _:c1 rdf:rest _:c2
             _:c2 rdf:first ex:partOf
             _:c2 rdf:rest rdf:nil");
        assert!(holds(&mut v, &result, "Pericarditis", "HeartDisease"));
        assert!(holds(&mut v, &result, "Pericarditis", "Disease"));
        assert!(holds(&mut v, &result, "Pericardium", "Tissue"));
        assert!(!holds(&mut v, &result, "HeartDisease", "Pericarditis"));
        assert!(result.unsatisfiable.is_empty());
        assert!(result.skipped.is_empty(), "{:?}", result.skipped);
    }

    /// The fast suite's el-classify witness (6 October 2026), delta-debugged from 20,000
    /// classes: `A ⊑ D` through the chain `r1 ∘ r2 ⊑ r3` with `r3 ⊑ r1` (an irregular
    /// hierarchy), applied twice. Entailed: x r1 y r2 z gives x r3 z, so x r1 z, and z r2 w
    /// gives x r3 w ⊑ r0 with w in A. ELK agrees once the input declares its terms; the
    /// hypertableau decides the regular variant (`r1 ∘ r2 ⊑ r1`) the same way.
    #[test]
    fn a_chain_into_a_sub_property_of_its_first_role_applies_again() {
        let axioms = "ex:A rdfs:subClassOf ex:A0
             ex:A rdfs:subClassOf _:e1
             _:e1 owl:onProperty ex:r6
             _:e1 owl:someValuesFrom ex:B
             ex:B rdfs:subClassOf _:e2
             _:e2 owl:onProperty ex:r8
             _:e2 owl:someValuesFrom ex:C
             ex:C rdfs:subClassOf _:e3
             _:e3 owl:onProperty ex:r4
             _:e3 owl:someValuesFrom ex:A
             ex:D owl:equivalentClass _:i
             _:i owl:intersectionOf _:l1
             _:l1 rdf:first ex:A0
             _:l1 rdf:rest _:l2
             _:l2 rdf:first _:e4
             _:l2 rdf:rest rdf:nil
             _:e4 owl:onProperty ex:r0
             _:e4 owl:someValuesFrom ex:A
             ex:r1 rdfs:subPropertyOf ex:r0
             ex:r3 rdfs:subPropertyOf ex:r1
             ex:r6 rdfs:subPropertyOf ex:r3
             ex:r4 rdfs:subPropertyOf ex:r2
             ex:r8 rdfs:subPropertyOf ex:r4";
        let chain = "ex:r3 owl:propertyChainAxiom _:c1
             _:c1 rdf:first ex:r1
             _:c1 rdf:rest _:c2
             _:c2 rdf:first ex:r2
             _:c2 rdf:rest rdf:nil";
        let (mut v, result) = run(&format!("{axioms}\n{chain}"));
        assert!(holds(&mut v, &result, "A", "D"));
        assert!(!holds(&mut v, &result, "B", "D") && !holds(&mut v, &result, "D", "A"));
        // Without the chain, A's r0-successors (B) aren't in A.
        let (mut v, result) = run(axioms);
        assert!(!holds(&mut v, &result, "A", "D"));
    }

    #[test]
    fn roles_domains_ranges_disjointness_and_transitivity() {
        let (mut v, result) = run("ex:hasParent rdfs:subPropertyOf ex:hasAncestor
             ex:hasAncestor rdf:type owl:TransitiveProperty
             ex:hasParent rdfs:domain ex:Person
             ex:hasParent rdfs:range ex:Person
             ex:Child rdfs:subClassOf _:r1
             _:r1 owl:onProperty ex:hasParent
             _:r1 owl:someValuesFrom ex:Parent
             ex:Parent rdfs:subClassOf _:r2
             _:r2 owl:onProperty ex:hasParent
             _:r2 owl:someValuesFrom ex:Elder
             ex:GrandChild owl:equivalentClass _:r3
             _:r3 owl:onProperty ex:hasAncestor
             _:r3 owl:someValuesFrom _:r4
             _:r4 owl:onProperty ex:hasAncestor
             _:r4 owl:someValuesFrom ex:Elder
             ex:PersonDescendant owl:equivalentClass _:r5
             _:r5 owl:onProperty ex:hasParent
             _:r5 owl:someValuesFrom ex:Person
             ex:Stone owl:disjointWith ex:Person
             ex:Golem rdfs:subClassOf ex:Stone
             ex:Golem rdfs:subClassOf ex:Child
             ex:Top owl:equivalentClass owl:Thing
             ex:Anything rdfs:subClassOf ex:Top");
        // Domain: a child has a parent, so it is a person.
        assert!(holds(&mut v, &result, "Child", "Person"));
        // Range: the parent is a person, so a child is a person-descendant.
        assert!(holds(&mut v, &result, "Child", "PersonDescendant"));
        // Transitivity: hasParent ⊑ hasAncestor, ancestor of an ancestor.
        assert!(holds(&mut v, &result, "Child", "GrandChild"));
        assert!(!holds(&mut v, &result, "Parent", "GrandChild"));
        // Disjointness: a golem would be a stone and a person.
        let golem = v.iri(&format!("{EX}Golem"));
        assert_eq!(result.unsatisfiable, vec![golem]);
        // owl:Thing is no superclass in the output; an equivalent of it is, and is
        // reported as one.
        assert!(holds(&mut v, &result, "Anything", "Top"));
        let top = v.iri(&format!("{EX}Top"));
        assert_eq!(result.top, vec![top]);
    }

    #[test]
    fn axioms_outside_el_are_skipped_and_counted() {
        let (mut v, result) = run("ex:A rdfs:subClassOf _:u
             _:u owl:unionOf _:l
             _:l rdf:first ex:B
             _:l rdf:rest rdf:nil
             ex:C rdfs:subClassOf _:a
             _:a owl:onProperty ex:p
             _:a owl:allValuesFrom ex:D
             ex:E rdfs:subClassOf ex:F");
        assert_eq!(result.skipped.len(), 2, "{:?}", result.skipped);
        assert!(holds(&mut v, &result, "E", "F"));
    }

    /// A class or property axiom the reader has no rule for is reported as omitted, never
    /// dropped (review s15-el-omissions r1 F1). With `r` reflexive, an `A` is its own
    /// `r`-successor, so `∃r.A ⊑ B` gives `A ⊑ B`; unread, `A ⊑ B` is missing and the
    /// omission says so. So are the axioms the rules would misread (I26 Sol r1): a top or
    /// bottom property read as a role (`owl:topObjectProperty rdfs:domain B` gives `B ≡
    /// owl:Thing`; `A ⊑ ∃owl:bottomObjectProperty.C`, `A ⊑ ⊥`), and like an inverse wherever
    /// a property axiom names it (a chain's subject, an equivalent property's object), a
    /// datatype read as a class
    /// or a restriction on a data property (`A ⊑ ∃p.xsd:integer` with `p`'s range
    /// `xsd:string`: `A ⊑ ⊥`), and a node that states two things: a field given twice, or
    /// two constructors, EL's or not (I26 Sol r2; PR #53 r1 F1, an existential beside a self
    /// restriction). Declarations, annotations, the ontology's header, a data property's
    /// domain and range (nothing about classes without a restriction; the top data
    /// property's domain makes its class `owl:Thing`, and is reported), and assertions about
    /// individuals and keys, which the classifier doesn't read, omit no such axiom.
    #[test]
    fn axioms_without_a_rule_are_reported_as_omitted() {
        /// `(predicate, reason)` omitted, or `(sub, super)` derived.
        type Pairs = &'static [(&'static str, &'static str)];
        let cases: [(&str, &str, Pairs, Pairs); 28] = [
            (
                "reflexive",
                "ex:r rdf:type owl:ReflexiveProperty
                 _:e owl:onProperty ex:r
                 _:e owl:someValuesFrom ex:A
                 _:e rdfs:subClassOf ex:B",
                &[("rdf:type", "reflexive property")],
                &[],
            ),
            (
                "disjoint-union",
                "ex:A owl:disjointUnionOf _:l0
                 _:l0 rdf:first ex:B
                 _:l0 rdf:rest _:l1
                 _:l1 rdf:first ex:C
                 _:l1 rdf:rest rdf:nil",
                &[("owl:disjointUnionOf", "disjoint union")],
                &[],
            ),
            (
                "inverse-axiom",
                "ex:p owl:inverseOf ex:q",
                &[("owl:inverseOf", "inverse property")],
                &[],
            ),
            (
                "inverse-sub-property",
                "_:i owl:inverseOf ex:p
                 _:i rdfs:subPropertyOf ex:q",
                &[("rdfs:subPropertyOf", "inverse property")],
                &[],
            ),
            (
                "inverse-transitive",
                "_:i owl:inverseOf ex:p
                 _:i rdf:type owl:TransitiveProperty",
                &[("rdf:type", "inverse property")],
                &[],
            ),
            (
                "named-complement",
                "ex:C owl:complementOf ex:A",
                &[("owl:complementOf", "complement")],
                &[],
            ),
            (
                "named-restriction",
                "ex:R owl:onProperty ex:r
                 ex:R owl:someValuesFrom ex:A",
                &[
                    ("owl:onProperty", "named class expression"),
                    ("owl:someValuesFrom", "named class expression"),
                ],
                &[],
            ),
            (
                "top-domain",
                "owl:topObjectProperty rdfs:domain ex:B",
                &[("rdfs:domain", "top or bottom property")],
                &[],
            ),
            (
                "bottom-restriction",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:onProperty owl:bottomObjectProperty
                 _:e owl:someValuesFrom ex:C",
                &[("rdfs:subClassOf", "top or bottom property")],
                &[],
            ),
            (
                "top-and-bottom-roles",
                "ex:p rdfs:subPropertyOf owl:topObjectProperty
                 owl:bottomObjectProperty rdf:type owl:TransitiveProperty
                 ex:r owl:propertyChainAxiom _:c
                 _:c rdf:first ex:s
                 _:c rdf:rest _:d
                 _:d rdf:first owl:topDataProperty
                 _:d rdf:rest rdf:nil
                 owl:topDataProperty rdfs:range ex:C
                 owl:bottomDataProperty owl:equivalentProperty ex:q",
                &[
                    ("rdfs:subPropertyOf", "top or bottom property"),
                    ("rdf:type", "top or bottom property"),
                    ("owl:propertyChainAxiom", "top or bottom property"),
                    ("rdfs:range", "top or bottom property"),
                    ("owl:equivalentProperty", "top or bottom property"),
                ],
                &[],
            ),
            (
                "unread-property-operands",
                "owl:topObjectProperty owl:propertyChainAxiom _:c
                 _:c rdf:first ex:r
                 _:c rdf:rest _:d
                 _:d rdf:first ex:s
                 _:d rdf:rest rdf:nil
                 _:i owl:inverseOf ex:q
                 ex:p owl:equivalentProperty _:i",
                &[
                    ("owl:propertyChainAxiom", "top or bottom property"),
                    ("owl:equivalentProperty", "inverse property"),
                ],
                &[],
            ),
            (
                "data-property",
                "ex:p rdf:type owl:DatatypeProperty
                 ex:p rdfs:range xsd:string
                 ex:A rdfs:subClassOf _:e
                 _:e owl:onProperty ex:p
                 _:e owl:someValuesFrom xsd:integer",
                &[("rdfs:subClassOf", "data restriction")],
                &[],
            ),
            (
                "datatype-filler",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:onProperty ex:q
                 _:e owl:someValuesFrom xsd:integer",
                &[("rdfs:subClassOf", "data range")],
                &[],
            ),
            (
                "declared-datatype",
                "ex:D rdf:type rdfs:Datatype
                 ex:q rdfs:range ex:D",
                &[("rdfs:range", "data range")],
                &[],
            ),
            (
                // A datatype read as a class is a data range, whatever its node also says.
                "declared-blank-datatype",
                "_:d rdf:type rdfs:Datatype
                 _:d owl:unionOf _:l
                 ex:A rdfs:subClassOf _:d",
                &[("rdfs:subClassOf", "data range")],
                &[],
            ),
            (
                "two-intersections",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:intersectionOf _:l
                 _:e owl:intersectionOf _:m
                 _:l rdf:first ex:C
                 _:l rdf:rest rdf:nil
                 _:m rdf:first ex:D
                 _:m rdf:rest rdf:nil
                 _:e owl:onProperty ex:r
                 _:e owl:someValuesFrom ex:B",
                &[("rdfs:subClassOf", "malformed class expression")],
                &[],
            ),
            (
                "intersection-and-two-fillers",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:intersectionOf _:l
                 _:l rdf:first ex:B
                 _:l rdf:rest _:m
                 _:m rdf:first ex:C
                 _:m rdf:rest rdf:nil
                 _:e owl:onProperty ex:r
                 _:e owl:someValuesFrom ex:D
                 _:e owl:someValuesFrom ex:E",
                &[("rdfs:subClassOf", "malformed class expression")],
                &[],
            ),
            (
                "intersection-and-existential",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:intersectionOf _:l
                 _:l rdf:first ex:B
                 _:l rdf:rest _:m
                 _:m rdf:first ex:C
                 _:m rdf:rest rdf:nil
                 _:e owl:onProperty ex:r
                 _:e owl:someValuesFrom ex:D",
                &[("rdfs:subClassOf", "malformed class expression")],
                &[],
            ),
            (
                "qualified-cardinality",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:onProperty ex:r
                 _:e owl:qualifiedCardinality ex:n
                 _:e owl:onClass ex:B",
                &[("rdfs:subClassOf", "cardinality")],
                &[],
            ),
            (
                "existential-and-self",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:onProperty ex:r
                 _:e owl:someValuesFrom ex:B
                 _:e owl:hasSelf \"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>",
                &[("rdfs:subClassOf", "malformed class expression")],
                &[],
            ),
            (
                "intersection-and-data-complement",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:intersectionOf _:l
                 _:l rdf:first ex:B
                 _:l rdf:rest _:m
                 _:m rdf:first ex:C
                 _:m rdf:rest rdf:nil
                 _:e owl:datatypeComplementOf ex:D",
                &[("rdfs:subClassOf", "malformed class expression")],
                &[],
            ),
            (
                "existential-and-n-ary",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:onProperty ex:r
                 _:e owl:someValuesFrom ex:B
                 _:e owl:onProperties _:l
                 _:l rdf:first ex:r
                 _:l rdf:rest rdf:nil",
                &[("rdfs:subClassOf", "n-ary data restriction")],
                &[],
            ),
            (
                "self-alone",
                "ex:A rdfs:subClassOf _:e
                 _:e owl:onProperty ex:r
                 _:e owl:hasSelf \"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>",
                &[("rdfs:subClassOf", "self restriction")],
                &[],
            ),
            (
                "named-self",
                "ex:S owl:hasSelf \"true\"^^<http://www.w3.org/2001/XMLSchema#boolean>",
                &[("owl:hasSelf", "self restriction")],
                &[],
            ),
            (
                "data-domain",
                "ex:p rdf:type owl:DatatypeProperty
                 ex:p rdfs:domain _:u
                 _:u owl:unionOf _:l
                 _:l rdf:first ex:A
                 _:l rdf:rest _:m
                 _:m rdf:first ex:B
                 _:m rdf:rest rdf:nil",
                &[],
                &[],
            ),
            (
                "data-domain-and-restriction",
                "ex:p rdf:type owl:DatatypeProperty
                 ex:p rdfs:domain _:u
                 _:u owl:unionOf _:l
                 _:l rdf:first ex:A
                 _:l rdf:rest _:m
                 _:m rdf:first ex:B
                 _:m rdf:rest rdf:nil
                 ex:C rdfs:subClassOf _:e
                 _:e owl:onProperty ex:p
                 _:e owl:someValuesFrom xsd:integer",
                &[("rdfs:subClassOf", "data restriction")],
                &[],
            ),
            (
                "top-data-domain",
                "owl:topDataProperty rdf:type owl:DatatypeProperty
                 owl:topDataProperty rdfs:domain ex:B",
                &[("rdfs:domain", "top or bottom property")],
                &[],
            ),
            (
                "no-axiom",
                "ex:o rdf:type owl:Ontology
                 ex:A rdf:type owl:Class
                 ex:A rdfs:label ex:l
                 ex:r rdf:type owl:ObjectProperty
                 ex:d rdf:type owl:DatatypeProperty
                 ex:d rdfs:range xsd:string
                 ex:D rdf:type rdfs:Datatype
                 ex:A rdfs:subClassOf ex:B
                 _:x rdf:type owl:Axiom
                 _:x owl:annotatedSource ex:A
                 _:x owl:annotatedProperty rdfs:subClassOf
                 _:x owl:annotatedTarget ex:B
                 _:x rdfs:comment ex:l
                 ex:A owl:hasKey _:k
                 _:k rdf:first ex:r
                 _:k rdf:rest rdf:nil
                 ex:a rdf:type owl:NamedIndividual
                 ex:a rdf:type ex:A
                 ex:a ex:r ex:b
                 ex:a owl:sameAs ex:c
                 ex:a owl:differentFrom ex:b",
                &[],
                &[("A", "B")],
            ),
        ];
        let mut failures = Vec::new();
        let mut check = |case: &str, input: &str, omitted: Pairs, subsumptions: Pairs| {
            let (mut v, actual) = run(input);
            let mut subsumptions: Vec<(u64, u64)> = subsumptions
                .iter()
                .map(|(a, b)| (v.iri(&format!("{EX}{a}")), v.iri(&format!("{EX}{b}"))))
                .collect();
            subsumptions.sort_unstable();
            let skipped = omitted
                .iter()
                .map(|&(predicate, reason)| {
                    let mut parts = load(&mut v, &format!("ex:X {predicate} ex:X"));
                    (parts.pop().unwrap()[1], reason)
                })
                .collect();
            let expected = Classification {
                subsumptions,
                skipped,
                ..Classification::default()
            };
            if actual != expected {
                failures.push(format!("{case}: expected {expected:?}, actual {actual:?}"));
            }
        };
        for (case, input, omitted, subsumptions) in cases {
            check(case, input, omitted, subsumptions);
        }
        // Each predicate of the mapping's class expressions and data ranges, given twice,
        // makes its node malformed, whatever the node would otherwise be read as (I26 Sol r2
        // S3; PR #53 r1 F1: the constructors EL doesn't read and their fields too).
        for field in [
            "intersectionOf",
            "someValuesFrom",
            "onProperty",
            "unionOf",
            "complementOf",
            "oneOf",
            "allValuesFrom",
            "hasValue",
            "hasSelf",
            "cardinality",
            "minCardinality",
            "maxCardinality",
            "qualifiedCardinality",
            "minQualifiedCardinality",
            "maxQualifiedCardinality",
            "onClass",
            "onDataRange",
            "onProperties",
            "inverseOf",
            "datatypeComplementOf",
            "onDatatype",
            "withRestrictions",
        ] {
            let input =
                format!("ex:A rdfs:subClassOf _:e\n_:e owl:{field} ex:X\n_:e owl:{field} ex:Y");
            check(
                &format!("two owl:{field}"),
                &input,
                &[("rdfs:subClassOf", "malformed class expression")],
                &[],
            );
        }
        // Each property characteristic and axiom no rule reads is reported with its kind,
        // stated by `rdf:type` or by its own predicate.
        for (class, kind) in [
            ("FunctionalProperty", "functional property"),
            ("InverseFunctionalProperty", "inverse-functional property"),
            ("ReflexiveProperty", "reflexive property"),
            ("IrreflexiveProperty", "irreflexive property"),
            ("SymmetricProperty", "symmetric property"),
            ("AsymmetricProperty", "asymmetric property"),
            ("AllDisjointProperties", "disjoint properties"),
        ] {
            let input = format!("ex:r rdf:type owl:{class}");
            check(&input, &input, vec![("rdf:type", kind)].leak(), &[]);
        }
        for (field, kind) in [
            ("propertyDisjointWith", "disjoint properties"),
            ("disjointUnionOf", "disjoint union"),
        ] {
            let predicate = format!("owl:{field}");
            let input = format!("ex:r {predicate} ex:q");
            check(&input, &input, vec![(&*predicate.leak(), kind)].leak(), &[]);
        }
        // Each predicate of the mapping's class expressions and data ranges on a named
        // subject defines that class (or relates two properties): reported, with the kind of
        // what EL lacks where it lacks the constructor.
        for (field, kind) in [
            ("intersectionOf", "named class expression"),
            ("someValuesFrom", "named class expression"),
            ("unionOf", "union"),
            ("complementOf", "complement"),
            ("oneOf", "nominal"),
            ("hasValue", "nominal"),
            ("allValuesFrom", "universal restriction"),
            ("hasSelf", "self restriction"),
            ("cardinality", "cardinality"),
            ("minCardinality", "cardinality"),
            ("maxCardinality", "cardinality"),
            ("qualifiedCardinality", "cardinality"),
            ("minQualifiedCardinality", "cardinality"),
            ("maxQualifiedCardinality", "cardinality"),
            ("inverseOf", "inverse property"),
            ("datatypeComplementOf", "data range"),
            ("onDatatype", "data range"),
            ("onProperty", "named class expression"),
            ("onClass", "named class expression"),
            ("onDataRange", "named class expression"),
            ("withRestrictions", "named class expression"),
            ("onProperties", "n-ary data restriction"),
        ] {
            let predicate = format!("owl:{field}");
            check(
                &format!("named {predicate}"),
                &format!("ex:N {predicate} ex:X"),
                vec![(&*predicate.clone().leak(), kind)].leak(),
                &[],
            );
        }
        // A restriction many axioms share as their subject (PR #53 sol-loop S1): read for
        // each, nothing to report, no subsumption between named classes. Its cost is the
        // counting probe's question, not this test's.
        let shared: String = (0..1_000)
            .map(|i| format!("_:e rdfs:subClassOf ex:C{i}\n"))
            .collect();
        check(
            "shared-restriction",
            &format!("_:e owl:onProperty ex:r\n_:e owl:someValuesFrom owl:Thing\n{shared}"),
            &[],
            &[],
        );
        // The same triple many times is one value (PR #53 sol-loop S2): a shared restriction
        // whose onProperty is repeated reads as the restriction; two different intersection
        // lists stay malformed for every axiom, however often the first is repeated.
        let referring: String = (0..200)
            .map(|i| format!("_:e rdfs:subClassOf ex:C{i}\n"))
            .collect();
        let copies = |triple: &str| triple.repeat(200);
        check(
            "repeated-onProperty",
            &format!(
                "{}_:e owl:someValuesFrom owl:Thing\n{referring}",
                copies("_:e owl:onProperty ex:r\n")
            ),
            &[],
            &[],
        );
        // The same on a node of a few pairs, which is scanned as read, not sorted.
        check(
            "onProperty-twice",
            "ex:A rdfs:subClassOf _:e
             _:e owl:onProperty ex:r
             _:e owl:onProperty ex:r
             _:e owl:someValuesFrom ex:B
             _:e rdfs:subClassOf ex:D",
            &[],
            &[("A", "D")],
        );
        let malformed: Pairs = vec![("rdfs:subClassOf", "malformed class expression"); 200].leak();
        check(
            "two-intersections-repeated",
            &format!(
                "_:e owl:intersectionOf _:l\n_:e owl:intersectionOf _:m\n{}\
                 _:l rdf:first ex:B\n_:l rdf:rest rdf:nil\n_:m rdf:first ex:D\n_:m rdf:rest rdf:nil\n\
                 {referring}",
                copies("_:e owl:intersectionOf _:l\n")
            ),
            malformed,
            &[],
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// An axiom whose list can't be read is reported as omitted, with why: malformed (a
    /// value missing, or two different ones where the mapping has one), cyclic, or past
    /// the reader's 10,001 members (a resource stop, which says nothing of the list's
    /// tail). The same triple twice is one. Below disjoint `A` and `B`, `X` is unsatisfiable; where their
    /// `AllDisjointClasses` can't be read, `X` stays satisfiable and the omission is
    /// reported, so no caller takes the result as complete.
    #[test]
    fn unreadable_lists_are_reported_as_omitted() {
        let disjoint = |n: usize| {
            let mut text = String::from(
                "ex:X rdf:type owl:Class\n\
                 _:d rdf:type owl:AllDisjointClasses\n\
                 _:d owl:members _:l0\n\
                 ex:X rdfs:subClassOf ex:A\n\
                 ex:X rdfs:subClassOf ex:B\n",
            );
            for i in 0..n {
                let member = match i {
                    0 => "ex:A".to_owned(),
                    1 => "ex:B".to_owned(),
                    _ => format!("ex:F{}", i - 2),
                };
                let rest = if i + 1 == n {
                    "rdf:nil".to_owned()
                } else {
                    format!("_:l{}", i + 1)
                };
                text.push_str(&format!(
                    "{member} rdf:type owl:Class\n\
                     _:l{i} rdf:first {member}\n_:l{i} rdf:rest {rest}\n"
                ));
            }
            text
        };
        let small = disjoint(3);
        let long = disjoint(10_002);
        let head = "_:d rdf:type owl:AllDisjointClasses\n_:d owl:members _:l0\n";
        let cases = [
            ("D3", small.clone(), None, true),
            (
                "missing-rest",
                small.replace("_:l2 rdf:rest rdf:nil\n", ""),
                Some(("rdf:type", "malformed list")),
                false,
            ),
            (
                "cycle",
                small.replace("_:l2 rdf:rest rdf:nil", "_:l2 rdf:rest _:l0"),
                Some(("rdf:type", "cyclic list")),
                false,
            ),
            (
                "missing-members",
                small.replace("_:d owl:members _:l0\n", ""),
                Some(("rdf:type", "malformed list")),
                false,
            ),
            (
                "two-members",
                small.replace(
                    "_:d owl:members _:l0\n",
                    "_:d owl:members _:l0\n_:d owl:members _:m0\n\
                     _:m0 rdf:first ex:A\n_:m0 rdf:rest rdf:nil\n",
                ),
                Some(("rdf:type", "malformed list")),
                false,
            ),
            (
                "two-firsts",
                small.replace(
                    "_:l0 rdf:first ex:A\n",
                    "_:l0 rdf:first ex:A\n_:l0 rdf:first ex:C\n",
                ),
                Some(("rdf:type", "malformed list")),
                false,
            ),
            (
                "a-triple-twice",
                small.replace(
                    "_:l0 rdf:first ex:A\n",
                    "_:l0 rdf:first ex:A\n_:l0 rdf:first ex:A\n",
                ),
                None,
                true,
            ),
            (
                "D10002",
                long.clone(),
                Some(("rdf:type", "list resource limit exceeded")),
                false,
            ),
            (
                "intersection-resource",
                long.replace(
                    head,
                    "ex:X rdfs:subClassOf _:i\n_:i owl:intersectionOf _:l0\n",
                ),
                Some(("rdfs:subClassOf", "list resource limit exceeded")),
                false,
            ),
            (
                "chain-resource",
                long.replace(head, "ex:r owl:propertyChainAxiom _:l0\n"),
                Some(("owl:propertyChainAxiom", "list resource limit exceeded")),
                false,
            ),
        ];
        let mut failures = Vec::new();
        for (case, input, omitted, bottom) in cases {
            let (mut v, actual) = run(&input);
            let x = v.iri(&format!("{EX}X"));
            let expected = Classification {
                subsumptions: if bottom {
                    Vec::new()
                } else {
                    let mut pairs =
                        vec![(x, v.iri(&format!("{EX}A"))), (x, v.iri(&format!("{EX}B")))];
                    pairs.sort_unstable();
                    pairs
                },
                unsatisfiable: if bottom { vec![x] } else { Vec::new() },
                top: Vec::new(),
                skipped: omitted
                    .map(|(predicate, reason)| {
                        let mut parts = load(&mut v, &format!("ex:X {predicate} ex:X"));
                        (parts.pop().unwrap()[1], reason)
                    })
                    .into_iter()
                    .collect(),
            };
            if actual != expected {
                failures.push(format!("{case}: expected {expected:?}, actual {actual:?}"));
            }
        }
        // The bound itself, through the list reader alone: 10,001 members are read
        // (classifying them would expand every pair).
        let mut v = LocalVocabulary::default();
        let triples = load(&mut v, &disjoint(10_001));
        let names = Names::new(&mut v);
        let snapshot = v.clone();
        let named = move |id: u64| snapshot.text(id).starts_with('<');
        let graph = Graph::new(&triples, &names, &named);
        assert_eq!(graph.list(v.term("_:l0")).unwrap().len(), 10_001);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// A datatype declared many times is one datatype: the reader holds as many as one
    /// declaration makes it hold, however often the declaration repeats (the input keeps
    /// duplicate lines).
    #[test]
    fn a_datatype_declared_many_times_is_held_once() {
        let held = |copies: usize| {
            let mut v = LocalVocabulary::default();
            let triples = load(&mut v, &"ex:D rdf:type rdfs:Datatype\n".repeat(copies));
            let names = Names::new(&mut v);
            let snapshot = v.clone();
            let named = move |id: u64| snapshot.text(id).starts_with('<');
            Graph::new(&triples, &names, &named).datatypes.slots.len()
        };
        assert_eq!(held(1_000), held(1));
    }

    /// An annotation property's domain, range and sub-properties have no logical meaning:
    /// no class is read from them, and nothing is left out for them. With `owl:Thing ⊑ T`,
    /// a class read from one would show as a subclass of `T`.
    #[test]
    fn an_annotation_propertys_axioms_name_no_class() {
        let (mut v, result) = run("ex:A rdf:type owl:Class
             ex:T rdf:type owl:Class
             owl:Thing rdfs:subClassOf ex:T
             ex:note rdf:type owl:AnnotationProperty
             ex:note rdfs:domain ex:X
             ex:source rdf:type owl:AnnotationProperty
             ex:source rdfs:range ex:Y
             ex:remark rdf:type owl:AnnotationProperty
             ex:remark rdfs:subPropertyOf ex:note");
        let [a, t] = ["A", "T"].map(|name| v.iri(&format!("{EX}{name}")));
        let expected = Classification {
            subsumptions: vec![(a, t)],
            top: vec![t],
            ..Classification::default()
        };
        assert_eq!(result, expected);
    }

    /// A name declared `owl:DataRange` is a datatype, as one declared `rdfs:Datatype` is (the
    /// RDF mapping reads OWL 1's `owl:DataRange` as `rdfs:Datatype`): an equivalence between
    /// two of them is no class axiom, and is left out as a data range.
    #[test]
    fn names_declared_owl_data_range_are_no_classes() {
        use std::collections::BTreeSet;
        let (mut v, result) = run("ex:Dt rdf:type owl:DataRange
             ex:Et rdf:type owl:DataRange
             ex:Dt owl:equivalentClass ex:Et
             ex:A rdf:type owl:Class
             ex:B rdf:type owl:Class
             ex:A rdfs:subClassOf ex:B");
        let [a, b] = ["A", "B"].map(|name| v.iri(&format!("{EX}{name}")));
        assert_eq!(result.subsumptions, vec![(a, b)]);
        assert!(result.top.is_empty() && result.unsatisfiable.is_empty());
        let skipped: BTreeSet<_> = result.skipped.into_iter().collect();
        let equivalent_class = v.iri(&format!("{OWL}equivalentClass"));
        assert_eq!(skipped, BTreeSet::from([(equivalent_class, "data range")]));
    }
}
