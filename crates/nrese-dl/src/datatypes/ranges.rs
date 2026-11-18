//! Data ranges as sets of values: each of the ontology's data ranges (and those the
//! engine adds: complements, a literal's singleton) evaluated once into `nrese-xsd`'s
//! value sets.
//!
//! Where a range can't be decided exactly, it is evaluated twice: a superset (`over`) and
//! a subset (`under`), each the right way round under complements. A clash found with the
//! supersets is a clash; a model found where a superset was used is no answer. Why it
//! isn't exact is one of two kinds:
//!
//! - `approximate`: the input isn't OWL 2 DL, or uses what the specification lets an
//!   implementation leave out: a datatype outside the map and not defined, a facet outside
//!   the datatype's facet space or a pattern that isn't a regular expression (*OWL 2
//!   Structural Specification* §11.2, §7.5); a cyclic datatype definition (§11.2); a
//!   literal whose value isn't represented here (a number past 128 bits, more than 18
//!   fractional digits of a second: §4 asks only for XML Schema's minimal lexical forms;
//!   an XML literal not decided);
//! - `limited`: a resource limit: two sets that together combine more patterns and
//!   language ranges than one set of values does (`MAX_PATTERNS`), each kind of text
//!   apart.

use std::collections::HashMap;

use nrese_owl::{DataRange, DataTerms, Interner, Normalised, Ontology, RangeId, Term};
use nrese_xsd::owl::{Datatype, Facet, LiteralError, Value, ValueSet, facet};

/// A data range's values: exactly (`over == under`), or between two sets.
#[derive(Debug, Clone)]
pub struct Eval {
    pub over: ValueSet,
    pub under: ValueSet,
    /// Why it isn't exact, if what it is is outside OWL 2 DL (or what the specification
    /// lets an implementation leave out).
    pub approximate: Option<String>,
    /// Why it isn't exact, if a resource limit is.
    pub limited: Option<String>,
}

/// Why two sets aren't combined.
pub const TOO_MANY_PATTERNS: &str =
    "a combination of more patterns and language ranges than one set of values holds";

impl Eval {
    fn exact(set: ValueSet) -> Self {
        Self {
            under: set.clone(),
            over: set,
            approximate: None,
            limited: None,
        }
    }

    fn unknown(why: String) -> Self {
        Self {
            over: ValueSet::all(),
            under: ValueSet::empty(),
            approximate: Some(why),
            limited: None,
        }
    }

    pub fn is_exact(&self) -> bool {
        self.approximate.is_none() && self.limited.is_none()
    }

    fn combines(&self, other: &Eval) -> bool {
        self.over.combines(&other.over) && self.under.combines(&other.under)
    }

    /// The intersection; past `MAX_PATTERNS`, bounded by this one's superset and the
    /// empty subset.
    fn and(self, other: &Eval) -> Self {
        let approximate = self
            .approximate
            .clone()
            .or_else(|| other.approximate.clone());
        let limited = self.limited.clone().or_else(|| other.limited.clone());
        if !self.combines(other) {
            return Self {
                over: self.over,
                under: ValueSet::empty(),
                approximate,
                limited: Some(TOO_MANY_PATTERNS.into()),
            };
        }
        Self {
            over: self.over.intersection(&other.over),
            under: self.under.intersection(&other.under),
            approximate,
            limited,
        }
    }

    /// The union; past `MAX_PATTERNS`, bounded by every value and this one's subset.
    fn or(self, other: &Eval) -> Self {
        let approximate = self
            .approximate
            .clone()
            .or_else(|| other.approximate.clone());
        let limited = self.limited.clone().or_else(|| other.limited.clone());
        if !self.combines(other) {
            return Self {
                over: ValueSet::all(),
                under: self.under,
                approximate,
                limited: Some(TOO_MANY_PATTERNS.into()),
            };
        }
        Self {
            over: self.over.union(&other.over),
            under: self.under.union(&other.under),
            approximate,
            limited,
        }
    }

    pub fn not(&self) -> Self {
        Self {
            over: self.under.complement(),
            under: self.over.complement(),
            approximate: self.approximate.clone(),
            limited: self.limited.clone(),
        }
    }
}

/// The data ranges of a program, evaluated.
#[derive(Debug, Clone, Default)]
pub struct Ranges {
    table: Interner<DataRange>,
    data: DataTerms,
    /// Datatype definitions: a datatype is its range.
    definitions: HashMap<Term, RangeId>,
    evals: Vec<Eval>,
}

impl Ranges {
    /// The ranges of `normalised` (of `ontology`), with its literals and definitions.
    pub fn new(ontology: &Ontology, normalised: &Normalised) -> Self {
        Self {
            table: normalised.ranges.clone(),
            data: ontology.data.clone(),
            definitions: normalised.definitions.iter().copied().collect(),
            evals: Vec::new(),
        }
    }

    /// The ranges of `normalised` with only the literals they name: for a reader of the
    /// ranges alone (the upper bound), not a copy of every literal the input asserts.
    pub fn of_ranges(ontology: &Ontology, normalised: &Normalised) -> Self {
        let mut ranges = Self::new(&Ontology::default(), normalised);
        ranges.data.iris = ontology.data.iris.clone();
        for id in 0..normalised.ranges.len() as u32 {
            let named: Vec<Term> = match normalised.ranges.get(id) {
                DataRange::OneOf(values) => values.clone(),
                DataRange::Restriction(_, facets) => facets.iter().map(|&(_, v)| v).collect(),
                _ => continue,
            };
            let literals = named
                .into_iter()
                .filter_map(|t| Some((t, ontology.data.literals.get(&t)?.clone())));
            ranges.data.literals.extend(literals);
        }
        ranges
    }

    /// Ranges from their parts (the theory's tests build them so).
    pub fn from_parts(
        table: Interner<DataRange>,
        data: DataTerms,
        definitions: HashMap<Term, RangeId>,
    ) -> Self {
        Self {
            table,
            data,
            definitions,
            evals: Vec::new(),
        }
    }

    pub fn intern(&mut self, range: DataRange) -> RangeId {
        RangeId(self.table.intern(range))
    }

    pub fn get(&self, id: RangeId) -> &DataRange {
        self.table.get(id.0)
    }

    /// Whether `id` is `rdfs:Literal` (every value: no constraint).
    pub fn is_literal(&self, id: RangeId) -> bool {
        matches!(self.get(id), DataRange::Literal)
    }

    /// Evaluates every range (call once all are interned).
    pub fn finish(&mut self) {
        let mut evals: Vec<Option<Eval>> = vec![None; self.table.len()];
        for id in 0..self.table.len() as u32 {
            self.compute(RangeId(id), &mut evals, &mut Vec::new());
        }
        self.evals = evals.into_iter().map(|e| e.expect("evaluated")).collect();
    }

    pub fn eval(&self, id: RangeId) -> &Eval {
        &self.evals[id.0 as usize]
    }

    /// The value of the literal `term`.
    pub fn value(&self, term: Term) -> Result<Value, String> {
        let Some(literal) = self.data.literals.get(&term) else {
            return Err(format!("term {term} isn't a literal the source gives"));
        };
        let iri = literal
            .datatype
            .as_deref()
            .ok_or_else(|| format!("the source gives no datatype of {:?}", literal.lexical))?;
        let datatype = Datatype::from_iri(iri)
            .ok_or_else(|| format!("{iri} isn't a datatype of the OWL 2 datatype map"))?;
        Value::parse(&literal.lexical, datatype, literal.language.as_deref()).map_err(|e| match e {
            LiteralError::IllTyped(why) => format!("an ill-typed literal: {why}"),
            LiteralError::Unsupported(why) => why,
        })
    }

    fn datatype(&self, term: Term) -> Result<Datatype, String> {
        let iri = self
            .data
            .iris
            .get(&term)
            .ok_or_else(|| format!("datatype term {term} has no IRI the source gives"))?;
        Datatype::from_iri(iri).ok_or_else(|| format!("{iri} isn't in the OWL 2 datatype map"))
    }

    fn compute(&self, id: RangeId, evals: &mut Vec<Option<Eval>>, stack: &mut Vec<Term>) -> Eval {
        if let Some(Some(e)) = evals.get(id.0 as usize) {
            return e.clone();
        }
        let eval = match self.get(id).clone() {
            DataRange::Literal => Eval::exact(ValueSet::all()),
            DataRange::Datatype(t) => match self.definitions.get(&t) {
                Some(_) if stack.contains(&t) => {
                    Eval::unknown("a cyclic datatype definition".into())
                }
                Some(&def) => {
                    stack.push(t);
                    let e = self.compute(def, evals, stack);
                    stack.pop();
                    e
                }
                None => match self.datatype(t) {
                    Ok(d) => Eval::exact(ValueSet::of(d)),
                    Err(why) => Eval::unknown(why),
                },
            },
            DataRange::And(xs) => {
                let mut e = Eval::exact(ValueSet::all());
                for x in xs {
                    let other = self.compute(x, evals, stack);
                    e = e.and(&other);
                }
                e
            }
            DataRange::Or(xs) => {
                let mut e = Eval::exact(ValueSet::empty());
                for x in xs {
                    let other = self.compute(x, evals, stack);
                    e = e.or(&other);
                }
                e
            }
            DataRange::Not(x) => self.compute(x, evals, stack).not(),
            DataRange::OneOf(literals) => {
                let mut e = Eval::exact(ValueSet::empty());
                for l in literals {
                    let one = match self.value(l) {
                        Ok(v) => Eval::exact(ValueSet::single(&v)),
                        Err(why) => Eval::unknown(why),
                    };
                    e = e.or(&one);
                }
                e
            }
            DataRange::Restriction(t, facets) => self.restriction(t, &facets),
        };
        if stack.is_empty() {
            evals[id.0 as usize] = Some(eval.clone());
        }
        eval
    }

    fn restriction(&self, t: Term, facets: &[(Term, Term)]) -> Eval {
        let d = match self.datatype(t) {
            Ok(d) => d,
            Err(why) => return Eval::unknown(why),
        };
        let mut e = Eval::exact(ValueSet::of(d));
        for &(f, v) in facets {
            let decided = (|| -> Result<ValueSet, String> {
                let iri = self
                    .data
                    .iris
                    .get(&f)
                    .ok_or_else(|| format!("facet term {f} has no IRI the source gives"))?;
                let facet_kind =
                    Facet::from_iri(iri).ok_or_else(|| format!("{iri} isn't a facet"))?;
                let value = self.value(v)?;
                facet(d, facet_kind, &value)
                    .ok_or_else(|| format!("the facet {iri} on {d:?} isn't decided"))
            })();
            match decided {
                Ok(set) if !(e.over.combines(&set) && e.under.combines(&set)) => {
                    // Past the patterns one set holds: without the facet, a superset.
                    e.under = ValueSet::empty();
                    e.limited.get_or_insert(TOO_MANY_PATTERNS.into());
                }
                Ok(set) => {
                    e.over = e.over.intersection(&set);
                    e.under = e.under.intersection(&set);
                }
                Err(why) => {
                    // Without the facet: a superset; nothing certain below it.
                    e.under = ValueSet::empty();
                    e.approximate.get_or_insert(why);
                }
            }
        }
        e
    }
}
