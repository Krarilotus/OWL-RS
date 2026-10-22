//! Admission for the existing tree representation, including repeated RDF subpaths.
//!
//! Only a completed path hands its payload charge to Shapes. Partial trees are freed
//! before the operation's one aggregate reservation, including on malformed input.

use super::{Compiler, UnaryPath, stopped};
use crate::graph::GraphView;
use crate::model::Path;
use nrese_engine::{TermId, TermKind};
use nrese_exec::{Budget, BudgetExceeded, ChargedVec, Reservation, Ticker};
use nrese_sparql::{QueryEvaluationError, ReadView};
use std::sync::Arc;

/// Existing syntax bound; it is not an output-size admission rule.
const MAX_PATH_DEPTH: usize = 64;

enum Problem {
    Malformed(&'static str),
    Evaluation(QueryEvaluationError),
}

impl From<BudgetExceeded> for Problem {
    fn from(error: BudgetExceeded) -> Self {
        Self::Evaluation(QueryEvaluationError::MemoryLimit(error))
    }
}

pub(super) fn compile<V: ReadView>(
    compiler: &mut Compiler<'_, V>,
    shape: TermId,
    node: TermId,
) -> Result<Option<Path>, QueryEvaluationError> {
    // The usual predicate path has no heap and needs no new token/Arc handle.
    if node.kind() == TermKind::Iri {
        compiler.ticker.tick(1).map_err(stopped)?;
        return Ok(Some(Path::Predicate(node)));
    }
    let mut held = Reservation::new(Arc::clone(&compiler.budget));
    let mut reader = Reader {
        graph: compiler.graph,
        ticker: &mut compiler.ticker,
        budget: &compiler.budget,
        held: &mut held,
        first: compiler.rdf_first,
        rest: compiler.rdf_rest,
        nil: compiler.rdf_nil,
    };
    match reader.path(node, 0) {
        Ok(path) => {
            compiler.held.absorb(held);
            Ok(Some(path))
        }
        Err(Problem::Malformed(message)) => {
            // No partial tree survives this arm. Only the ordinary diagnostic escapes.
            drop(held);
            compiler.error(shape, message)?;
            Ok(None)
        }
        Err(Problem::Evaluation(error)) => Err(error),
    }
}

struct Reader<'a, V: ReadView> {
    graph: GraphView<'a, V>,
    ticker: &'a mut Ticker,
    budget: &'a Arc<Budget>,
    held: &'a mut Reservation,
    first: Option<TermId>,
    rest: Option<TermId>,
    nil: Option<TermId>,
}

impl<'a, V: ReadView> Reader<'a, V> {
    fn tick(&mut self) -> Result<(), Problem> {
        self.ticker
            .tick(1)
            .map_err(|stop| Problem::Evaluation(stopped(stop)))
    }

    /// Set cardinality, without allocating or sorting an objects vector. Duplicate
    /// statements across selected graphs still count as one value.
    fn one(&mut self, node: TermId, predicate: Option<TermId>) -> Result<Option<TermId>, Problem> {
        let Some(predicate) = predicate else {
            return Ok(None);
        };
        let mut value = None;
        for quad in self.graph.scan(Some(node), Some(predicate), None) {
            self.tick()?;
            match value {
                None => value = Some(quad.object),
                Some(first) if first != quad.object => return Ok(None),
                _ => {}
            }
        }
        Ok(value)
    }

    fn value(&mut self, node: TermId, local: &str) -> Result<Option<TermId>, Problem> {
        self.one(
            node,
            self.graph.iri(&format!("{}{local}", crate::model::SH)),
        )
    }

    fn list(&mut self, head: TermId) -> Result<ChargedVec<TermId, &'a Budget>, Problem> {
        let mut members = ChargedVec::new(&**self.budget);
        let mut node = head;
        // Brent's single-pass cycle check needs no visited-node allocation or second
        // walk. Validate the complete list before its members, as the old parser did.
        let mut anchor = head;
        let mut power = 1_usize;
        let mut distance = 0_usize;
        while Some(node) != self.nil {
            self.tick()?;
            if distance == power {
                anchor = node;
                power = power.saturating_mul(2);
                distance = 0;
            }
            let (Some(first), Some(rest)) = (self.first, self.rest) else {
                return Err(Problem::Malformed("sh:path isn't a list"));
            };
            let (Some(member), Some(next)) =
                (self.one(node, Some(first))?, self.one(node, Some(rest))?)
            else {
                return Err(Problem::Malformed("sh:path isn't a well-formed list"));
            };
            members.push(member)?;
            node = next;
            distance += 1;
            if Some(node) != self.nil && node == anchor {
                return Err(Problem::Malformed("sh:path is a cyclic list"));
            }
        }
        Ok(members)
    }

    fn members(&mut self, head: TermId, depth: usize) -> Result<Vec<Path>, Problem> {
        // The temporary ID vector and the final Path vector overlap and are both held.
        let members = self.list(head)?;
        if members.len() < 2 {
            return Err(Problem::Malformed(
                "a sequence or alternative path needs two members",
            ));
        }
        let mut paths = ChargedVec::with_capacity(Arc::clone(self.budget), members.len())?;
        for &member in members.iter() {
            paths.push(self.path(member, depth + 1)?)?;
        }
        let (paths, held) = paths.into_parts();
        self.held.absorb(held);
        Ok(paths)
    }

    fn path(&mut self, node: TermId, depth: usize) -> Result<Path, Problem> {
        self.tick()?;
        if depth > MAX_PATH_DEPTH {
            return Err(Problem::Malformed("sh:path is nested too deeply or cyclic"));
        }
        match node.kind() {
            TermKind::Iri => return Ok(Path::Predicate(node)),
            TermKind::BlankNode => {}
            _ => return Err(Problem::Malformed("sh:path isn't an IRI or a path")),
        }
        // Any rdf:first makes this a sequence, even if other path properties exist.
        // Stop after its first statement; no objects vector is needed for existence.
        if let Some(first) = self.first
            && self
                .graph
                .scan(Some(node), Some(first), None)
                .next()
                .is_some()
        {
            self.tick()?;
            return Ok(Path::Sequence(self.members(node, depth)?));
        }
        let unary: [(&str, UnaryPath); 4] = [
            ("inversePath", Path::Inverse),
            ("zeroOrMorePath", Path::ZeroOrMore),
            ("oneOrMorePath", Path::OneOrMore),
            ("zeroOrOnePath", Path::ZeroOrOne),
        ];
        for (local, make) in unary {
            if let Some(inner) = self.value(node, local)? {
                let inner = self.path(inner, depth + 1)?;
                self.held.charge(size_of::<Path>())?;
                return Ok(make(Box::new(inner)));
            }
        }
        if let Some(alternatives) = self.value(node, "alternativePath")? {
            return Ok(Path::Alternative(self.members(alternatives, depth)?));
        }
        Err(Problem::Malformed("sh:path isn't an IRI or a path"))
    }
}
