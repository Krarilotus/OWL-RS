//! Independent active-domain evaluator for OWL 2 Profiles §4.3, Tables 4–9.

use std::collections::{BTreeSet, HashMap};

use crate::{Facts, Triple};

type Id = usize;
type Row = [Id; 3];
const UNBOUND: Id = 0;
const TRIPLE: usize = 0;
const FALSE: usize = 1;

pub struct Model {
    pub facts: Facts,
    pub inconsistent: bool,
}

#[derive(Clone, Copy)]
pub enum Datatypes {
    Standard,
    WithoutType2,
    Published,
}

#[derive(Clone, Copy)]
enum Term {
    Constant(Id),
    Variable(usize),
}

struct Atom {
    relation: usize,
    terms: [Term; 3],
}

struct Rule {
    body: Vec<Atom>,
    heads: Vec<Atom>,
    variables: usize,
}

#[derive(Default)]
struct Symbols {
    ids: HashMap<String, Id>,
    names: Vec<String>,
}

impl Symbols {
    fn id(&mut self, name: &str) -> Id {
        *self.ids.entry(name.to_owned()).or_insert_with(|| {
            self.names.push(name.to_owned());
            self.names.len()
        })
    }

    fn name(&self, id: Id) -> &str {
        &self.names[id - 1]
    }
}

struct Program {
    symbols: Symbols,
    relations: Symbols,
    rules: Vec<Rule>,
    padding: Id,
}

impl Program {
    fn read() -> Self {
        let mut symbols = Symbols::default();
        let padding = symbols.id("");
        let mut relations = Symbols::default();
        assert_eq!(relations.id("T") - 1, TRIPLE);
        assert_eq!(relations.id("@false") - 1, FALSE);
        let mut program = Self {
            symbols,
            relations,
            rules: Vec::new(),
            padding,
        };
        for line in include_str!("rules.txt")
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let (name, clauses) = line.split_once('|').unwrap();
            let (body, heads) = clauses.split_once('|').unwrap_or_else(|| panic!("{name}"));
            let mut variables = Symbols::default();
            let body = body
                .split(',')
                .filter(|atom| !atom.trim().is_empty())
                .map(|atom| program.atom(atom, &mut variables))
                .collect();
            let heads = heads
                .split(';')
                .map(|atom| program.atom(atom, &mut variables))
                .collect();
            program.rules.push(Rule {
                body,
                heads,
                variables: variables.names.len(),
            });
        }
        program
    }

    fn atom(&mut self, text: &str, variables: &mut Symbols) -> Atom {
        let mut tokens = text.split_whitespace().peekable();
        let relation = match tokens.peek() {
            Some(token) if token.starts_with('@') => self.relations.id(tokens.next().unwrap()) - 1,
            _ => TRIPLE,
        };
        let mut terms = [Term::Constant(self.padding); 3];
        for (index, token) in tokens.enumerate() {
            terms[index] = match token.strip_prefix('?') {
                Some(name) => Term::Variable(variables.id(name) - 1),
                None => Term::Constant(self.symbols.id(token)),
            };
        }
        Atom { relation, terms }
    }
}

#[derive(Default)]
struct Relation {
    facts: BTreeSet<Row>,
    rows: Vec<Row>,
    columns: [HashMap<Id, Vec<Row>>; 3],
}

impl Relation {
    fn insert(&mut self, row: Row) -> bool {
        if !self.facts.insert(row) {
            return false;
        }
        self.rows.push(row);
        for (column, key) in self.columns.iter_mut().zip(row) {
            column.entry(key).or_default().push(row);
        }
        true
    }

    fn candidates(&self, keys: Row) -> &[Row] {
        let mut best = self.rows.as_slice();
        for (column, key) in self.columns.iter().zip(keys) {
            if key != UNBOUND {
                let bucket = column.get(&key).map_or(&[][..], Vec::as_slice);
                if bucket.len() < best.len() {
                    best = bucket;
                }
            }
        }
        best
    }
}

impl Atom {
    fn keys(&self, bindings: &[Id]) -> Row {
        self.terms.map(|term| match term {
            Term::Constant(value) => value,
            Term::Variable(index) => bindings[index],
        })
    }

    fn bind(&self, row: Row, bindings: &mut [Id]) -> bool {
        self.terms.iter().zip(row).all(|(term, value)| match *term {
            Term::Constant(constant) => constant == value,
            Term::Variable(index) => {
                let bound = &mut bindings[index];
                if *bound == UNBOUND {
                    *bound = value;
                }
                *bound == value
            }
        })
    }
}

impl Rule {
    fn join(
        &self,
        database: &[Relation],
        anchor: usize,
        next: usize,
        bindings: &[Id],
        derived: &mut Vec<(usize, Row)>,
    ) {
        if next == self.body.len() {
            derived.extend(self.heads.iter().map(|head| {
                let row = head.keys(bindings);
                debug_assert!(!row.contains(&UNBOUND));
                (head.relation, row)
            }));
            return;
        }
        if next == anchor {
            self.join(database, anchor, next + 1, bindings, derived);
            return;
        }
        let atom = &self.body[next];
        let mut extended = bindings.to_vec();
        for &row in database[atom.relation].candidates(atom.keys(bindings)) {
            extended.copy_from_slice(bindings);
            if atom.bind(row, &mut extended) {
                self.join(database, anchor, next + 1, &extended, derived);
            }
        }
    }
}

pub fn evaluate(asserted: &Facts, datatypes: Datatypes) -> Model {
    let mut program = Program::read();
    let mut initial = asserted.clone();
    let bad_types = datatype_facts(asserted, datatypes, &mut initial);
    // Table 5, prp-ap. OWL 2 Structural Specification §5.5 gives the built-in properties.
    for annotation in [
        "rdfs:label",
        "rdfs:comment",
        "rdfs:seeAlso",
        "rdfs:isDefinedBy",
        "owl:deprecated",
        "owl:versionInfo",
        "owl:priorVersion",
        "owl:backwardCompatibleWith",
        "owl:incompatibleWith",
    ] {
        initial.insert(crate::triple([
            annotation,
            "rdf:type",
            "owl:AnnotationProperty",
        ]));
    }
    // Table 8, dt-not-type: a type stating a value is outside that datatype derives false.
    let mut variables = Symbols::default();
    let body = ["?literal rdf:type ?dt", "@outside ?literal ?dt"]
        .map(|text| program.atom(text, &mut variables));
    let head = program.atom("@false", &mut variables);
    program.rules.push(Rule {
        body: Vec::from(body),
        heads: vec![head],
        variables: variables.names.len(),
    });
    let outside = program.relations.id("@outside") - 1;
    let mut database: Vec<_> = (0..program.relations.names.len())
        .map(|_| Relation::default())
        .collect();
    for triple in initial {
        database[TRIPLE].insert(triple.map(|term| program.symbols.id(&term)));
    }
    for (literal, datatype) in bad_types {
        database[outside].insert([
            program.symbols.id(&literal),
            program.symbols.id(datatype),
            program.padding,
        ]);
    }

    // Each round visits every rule instance with at least one new premise. The first
    // round also visits empty-body rules. Updating relations only at the round boundary
    // makes the result independent of rule and input order.
    let mut previous = vec![0; database.len()];
    let mut first = true;
    loop {
        let mut derived = Vec::new();
        for rule in &program.rules {
            let mut bindings = vec![UNBOUND; rule.variables];
            if first && rule.body.is_empty() {
                rule.join(&database, usize::MAX, 0, &bindings, &mut derived);
            }
            for (anchor, atom) in rule.body.iter().enumerate() {
                for &row in &database[atom.relation].rows[previous[atom.relation]..] {
                    bindings.fill(UNBOUND);
                    if atom.bind(row, &mut bindings) {
                        rule.join(&database, anchor, 0, &bindings, &mut derived);
                    }
                }
            }
        }
        first = false;
        for (length, relation) in previous.iter_mut().zip(&database) {
            *length = relation.rows.len();
        }
        let mut changed = false;
        for (relation, row) in derived {
            changed |= database[relation].insert(row);
        }
        if !changed {
            break;
        }
    }
    Model {
        facts: database[TRIPLE]
            .rows
            .iter()
            .map(|row| row.map(|id| program.symbols.name(id).to_owned()))
            .collect(),
        inconsistent: !database[FALSE].rows.is_empty(),
    }
}

struct Space {
    datatype: &'static str,
    bounds: Option<(i128, i128)>,
}

// Numeric value spaces in Table 8. Bounds apply only to integral values.
const NUMERIC: &[Space] = &[
    Space {
        datatype: "xsd:decimal",
        bounds: None,
    },
    Space {
        datatype: "xsd:integer",
        bounds: Some((i128::MIN, i128::MAX)),
    },
    Space {
        datatype: "xsd:nonNegativeInteger",
        bounds: Some((0, i128::MAX)),
    },
    Space {
        datatype: "xsd:nonPositiveInteger",
        bounds: Some((i128::MIN, 0)),
    },
    Space {
        datatype: "xsd:positiveInteger",
        bounds: Some((1, i128::MAX)),
    },
    Space {
        datatype: "xsd:negativeInteger",
        bounds: Some((i128::MIN, -1)),
    },
    Space {
        datatype: "xsd:long",
        bounds: Some((i64::MIN as i128, i64::MAX as i128)),
    },
    Space {
        datatype: "xsd:int",
        bounds: Some((i32::MIN as i128, i32::MAX as i128)),
    },
    Space {
        datatype: "xsd:short",
        bounds: Some((i16::MIN as i128, i16::MAX as i128)),
    },
    Space {
        datatype: "xsd:byte",
        bounds: Some((i8::MIN as i128, i8::MAX as i128)),
    },
    Space {
        datatype: "xsd:unsignedLong",
        bounds: Some((0, u64::MAX as i128)),
    },
    Space {
        datatype: "xsd:unsignedInt",
        bounds: Some((0, u32::MAX as i128)),
    },
    Space {
        datatype: "xsd:unsignedShort",
        bounds: Some((0, u16::MAX as i128)),
    },
    Space {
        datatype: "xsd:unsignedByte",
        bounds: Some((0, u8::MAX as i128)),
    },
];

const OTHER_DATATYPES: &[&str] = &[
    "rdf:PlainLiteral",
    "rdf:XMLLiteral",
    "rdfs:Literal",
    "xsd:float",
    "xsd:double",
    "xsd:string",
    "xsd:normalizedString",
    "xsd:token",
    "xsd:language",
    "xsd:Name",
    "xsd:NCName",
    "xsd:NMTOKEN",
    "xsd:boolean",
    "xsd:hexBinary",
    "xsd:base64Binary",
    "xsd:anyURI",
    "xsd:dateTime",
    "xsd:dateTimeStamp",
];

#[derive(PartialEq, Eq)]
struct Number<'a> {
    integer: i128,
    fraction: &'a str,
    negative: bool,
}

impl<'a> Number<'a> {
    // The fixture vocabulary is small integer and decimal literals, plus RDF-mapped
    // nonnegative integer cardinalities. Fail if a fixture expands that vocabulary.
    fn parse(term: &'a str) -> Self {
        let (lexical, datatype) = term[1..].split_once("\"^^").unwrap();
        let (lexical, negative) = match lexical.as_bytes().first() {
            Some(b'-') => (&lexical[1..], true),
            Some(b'+') => (&lexical[1..], false),
            _ => (lexical, false),
        };
        let (whole, fraction) = lexical.split_once('.').unwrap_or((lexical, ""));
        assert!(
            matches!(
                datatype,
                "xsd:decimal" | "xsd:integer" | "xsd:nonNegativeInteger"
            ) && (datatype == "xsd:decimal" || !lexical.contains('.'))
                && (!whole.is_empty() || !fraction.is_empty())
                && whole
                    .bytes()
                    .chain(fraction.bytes())
                    .all(|b| b.is_ascii_digit()),
            "unsupported fixture literal {term}"
        );
        let whole = if whole.is_empty() { "0" } else { whole };
        let magnitude: i128 = whole.parse().expect("small fixture value");
        let fraction = fraction.trim_end_matches('0');
        let negative = negative && (magnitude != 0 || !fraction.is_empty());
        let integer = if negative { -magnitude } else { magnitude };
        assert!(datatype != "xsd:nonNegativeInteger" || !negative);
        Self {
            integer,
            fraction,
            negative,
        }
    }

    fn in_space(&self, datatype: &str) -> bool {
        datatype == "rdfs:Literal"
            || NUMERIC.iter().any(|space| {
                space.datatype == datatype
                    && space.bounds.is_none_or(|(min, max)| {
                        self.fraction.is_empty() && (min..=max).contains(&self.integer)
                    })
            })
    }
}

fn datatype_facts(
    asserted: &Facts,
    rules: Datatypes,
    initial: &mut BTreeSet<Triple>,
) -> Vec<(String, &'static str)> {
    let literals: Vec<_> = asserted
        .iter()
        .flatten()
        .filter(|term| term.starts_with('"'))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|term| (term, Number::parse(term)))
        .collect();
    let mut bad_types = Vec::new();
    for datatype in NUMERIC
        .iter()
        .map(|space| space.datatype)
        .chain(OTHER_DATATYPES.iter().copied())
    {
        // Table 8, dt-type1
        initial.insert(crate::triple([datatype, "rdf:type", "rdfs:Datatype"]));
        for (literal, value) in &literals {
            if value.in_space(datatype) {
                // Table 8, dt-type2
                if matches!(rules, Datatypes::Standard) {
                    initial.insert(crate::triple([literal, "rdf:type", datatype]));
                }
            } else {
                bad_types.push(((*literal).clone(), datatype));
            }
        }
    }
    for (left, a) in &literals {
        for (right, b) in &literals {
            // Table 8, dt-eq and dt-diff
            if a != b {
                initial.insert(crate::triple([left, "owl:differentFrom", right]));
            } else if !matches!(rules, Datatypes::Published) {
                initial.insert(crate::triple([left, "owl:sameAs", right]));
            }
        }
    }
    bad_types
}
