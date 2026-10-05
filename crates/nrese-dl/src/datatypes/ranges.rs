//! Data ranges as sets of values: each of the ontology's data ranges (and those the
//! engine adds: complements, a literal's singleton) evaluated once into `nrese-xsd`'s
//! value sets.
//!
//! Where a range can't be decided exactly (a datatype outside the map, a pattern of binary
//! data, a literal whose value isn't represented, an ontology with more distinct patterns
//! and language ranges than one set of values combines, `MAX_PATTERNS`), it is evaluated
//! twice: a
//! superset (`over`) and a subset (`under`), each the right way round under complements.
//! A clash found with the supersets is a clash; a model found where a superset was used
//! is no answer (`approximate` says why).

use std::collections::{HashMap, HashSet};

use nrese_owl::{DataRange, DataTerms, Interner, Normalised, Ontology, RangeId, Term};
use nrese_xsd::owl::{Datatype, Facet, LiteralError, MAX_PATTERNS, Value, ValueSet, facet};

/// A data range's values: exactly (`over == under`), or between two sets.
#[derive(Debug, Clone)]
pub struct Eval {
    pub over: ValueSet,
    pub under: ValueSet,
    /// Why it isn't exact, if it isn't.
    pub approximate: Option<String>,
}

impl Eval {
    fn exact(set: ValueSet) -> Self {
        Self {
            under: set.clone(),
            over: set,
            approximate: None,
        }
    }

    fn unknown(why: String) -> Self {
        Self {
            over: ValueSet::all(),
            under: ValueSet::empty(),
            approximate: Some(why),
        }
    }

    pub fn is_exact(&self) -> bool {
        self.approximate.is_none()
    }

    fn and(self, other: &Eval) -> Self {
        Self {
            over: self.over.intersection(&other.over),
            under: self.under.intersection(&other.under),
            approximate: self.approximate.or_else(|| other.approximate.clone()),
        }
    }

    fn or(self, other: &Eval) -> Self {
        Self {
            over: self.over.union(&other.over),
            under: self.under.union(&other.under),
            approximate: self.approximate.or_else(|| other.approximate.clone()),
        }
    }

    pub fn not(&self) -> Self {
        Self {
            over: self.under.complement(),
            under: self.over.complement(),
            approximate: self.approximate.clone(),
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
    /// More distinct patterns and language ranges than a set combines: they aren't decided.
    too_many_patterns: bool,
}

impl Ranges {
    /// The ranges of `normalised` (of `ontology`), with its literals and definitions.
    pub fn new(ontology: &Ontology, normalised: &Normalised) -> Self {
        Self {
            table: normalised.ranges.clone(),
            data: ontology.data.clone(),
            definitions: normalised.definitions.iter().copied().collect(),
            evals: Vec::new(),
            too_many_patterns: false,
        }
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
            too_many_patterns: false,
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
        let patterns: HashSet<(Term, Term)> = (0..self.table.len() as u32)
            .filter_map(|id| match self.get(RangeId(id)) {
                DataRange::Restriction(_, facets) => Some(facets.clone()),
                _ => None,
            })
            .flatten()
            .filter(|(f, _)| {
                self.data
                    .iris
                    .get(f)
                    .and_then(|iri| Facet::from_iri(iri))
                    .is_some_and(|f| matches!(f, Facet::Pattern | Facet::LangRange))
            })
            .collect();
        self.too_many_patterns = patterns.len() > MAX_PATTERNS;
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
                if self.too_many_patterns && matches!(facet_kind, Facet::Pattern | Facet::LangRange)
                {
                    return Err(format!(
                        "more than {MAX_PATTERNS} patterns and language ranges in the ontology"
                    ));
                }
                let value = self.value(v)?;
                facet(d, facet_kind, &value)
                    .ok_or_else(|| format!("the facet {iri} on {d:?} isn't decided"))
            })();
            match decided {
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
