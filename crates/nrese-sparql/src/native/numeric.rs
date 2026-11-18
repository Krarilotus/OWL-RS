//! SPARQL numeric promotion and arithmetic shared by expressions and aggregates.
//! XSD supplies the value operations; OWL datatype reasoning and calendar arithmetic
//! retain their own semantics. Promotion is constant work and allocates no terms.

use nrese_rdf::vocab::xsd;
use nrese_rdf::{Literal, Term};
use nrese_sparql_syntax::algebra::Function;
use nrese_xsd::{Decimal, Double, Float, Integer};

use super::value::Value;

#[derive(Clone, Copy)]
pub(super) enum Operator {
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// A numeric value, for arithmetic with SPARQL's type promotion
/// (integer → decimal → float → double); results print in the XSD value's string form.
#[derive(Clone, Copy)]
pub(super) enum Numeric {
    Integer(Integer),
    Decimal(Decimal),
    Float(Float),
    Double(Double),
}

impl Numeric {
    pub(super) fn of(value: &Value) -> Option<Self> {
        Some(match value {
            Value::Integer(i) => Self::Integer(*i),
            Value::Decimal(d) => Self::Decimal(*d),
            Value::Float(f) => Self::Float(*f),
            Value::Double(d) => Self::Double(*d),
            _ => return None,
        })
    }

    pub(super) fn term(self) -> Term {
        match self {
            Self::Integer(i) => Literal::new_typed_literal(i.to_string(), xsd::INTEGER),
            Self::Decimal(d) => Literal::new_typed_literal(d.to_string(), xsd::DECIMAL),
            Self::Float(f) => Literal::new_typed_literal(f.to_string(), xsd::FLOAT),
            Self::Double(d) => Literal::new_typed_literal(d.to_string(), xsd::DOUBLE),
        }
        .into()
    }

    pub(super) fn negate(self) -> Option<Term> {
        Some(
            match self {
                Self::Integer(i) => Self::Integer(i.checked_neg()?),
                Self::Decimal(d) => Self::Decimal(d.checked_neg()?),
                Self::Float(f) => Self::Float(-f),
                Self::Double(d) => Self::Double(-d),
            }
            .term(),
        )
    }

    pub(super) fn rounding(self, function: &Function) -> Option<Term> {
        Some(
            match (self, function) {
                (Self::Integer(i), Function::Abs) => Self::Integer(i.checked_abs()?),
                (Self::Integer(i), _) => Self::Integer(i),
                (Self::Decimal(d), Function::Abs) => Self::Decimal(d.checked_abs()?),
                (Self::Decimal(d), Function::Ceil) => Self::Decimal(d.checked_ceil()?),
                (Self::Decimal(d), Function::Floor) => Self::Decimal(d.checked_floor()?),
                (Self::Decimal(d), _) => Self::Decimal(d.checked_round()?),
                (Self::Float(f), Function::Abs) => Self::Float(f.abs()),
                (Self::Float(f), Function::Ceil) => Self::Float(f.ceil()),
                (Self::Float(f), Function::Floor) => Self::Float(f.floor()),
                (Self::Float(f), _) => Self::Float(f.round()),
                (Self::Double(d), Function::Abs) => Self::Double(d.abs()),
                (Self::Double(d), Function::Ceil) => Self::Double(d.ceil()),
                (Self::Double(d), Function::Floor) => Self::Double(d.floor()),
                (Self::Double(d), _) => Self::Double(d.round()),
            }
            .term(),
        )
    }

    pub(super) fn decimal(self) -> Option<Decimal> {
        match self {
            Self::Integer(i) => Some(Decimal::from(i)),
            Self::Decimal(d) => Some(d),
            _ => None,
        }
    }

    fn float(self) -> Option<Float> {
        match self {
            Self::Integer(i) => Some(Float::from(i)),
            Self::Decimal(d) => Some(Float::from(d)),
            Self::Float(f) => Some(f),
            Self::Double(_) => None,
        }
    }

    fn double(self) -> Double {
        match self {
            Self::Integer(i) => Double::from(i),
            Self::Decimal(d) => Double::from(d),
            Self::Float(f) => Double::from(f),
            Self::Double(d) => d,
        }
    }

    pub(super) fn add(self, other: Self) -> Option<Self> {
        self.arithmetic(Operator::Add, other)
    }

    pub(super) fn arithmetic(self, operator: Operator, other: Self) -> Option<Self> {
        use Numeric::{Decimal as D, Double as Db, Float as F, Integer as I};
        let result = match (self, other) {
            (I(x), I(y)) => match operator {
                Operator::Add => I(x.checked_add(y)?),
                Operator::Subtract => I(x.checked_sub(y)?),
                Operator::Multiply => I(x.checked_mul(y)?),
                // Integer division is decimal division in SPARQL.
                Operator::Divide => D(Decimal::from(x).checked_div(Decimal::from(y))?),
            },
            (I(_) | D(_), I(_) | D(_)) => {
                let (x, y) = (self.decimal()?, other.decimal()?);
                D(match operator {
                    Operator::Add => x.checked_add(y)?,
                    Operator::Subtract => x.checked_sub(y)?,
                    Operator::Multiply => x.checked_mul(y)?,
                    Operator::Divide => x.checked_div(y)?,
                })
            }
            (I(_) | D(_) | F(_), I(_) | D(_) | F(_)) => {
                let (x, y) = (self.float()?, other.float()?);
                F(match operator {
                    Operator::Add => x + y,
                    Operator::Subtract => x - y,
                    Operator::Multiply => x * y,
                    Operator::Divide => x / y,
                })
            }
            _ => {
                let (x, y) = (self.double(), other.double());
                Db(match operator {
                    Operator::Add => x + y,
                    Operator::Subtract => x - y,
                    Operator::Multiply => x * y,
                    Operator::Divide => x / y,
                })
            }
        };
        Some(result)
    }
}
