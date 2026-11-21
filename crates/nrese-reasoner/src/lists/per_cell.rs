use std::collections::HashMap;
use std::sync::Arc;

use nrese_engine::term::PAYLOAD_BITS;

use super::{Cell, ListKind, ListVocabulary, c, diagonal, rule, v};
use crate::ir::{Atom, Guard, Head, Rule, Triple};

// Tags 13–15 name relations private to the reasoner. A cell may be a generalised
// RDF term, so allocate a payload rather than discarding the term's original tag.
const CHAIN: u64 = 13 << PAYLOAD_BITS;
const KEY: u64 = 14 << PAYLOAD_BITS;
const INTERSECTION: u64 = 15 << PAYLOAD_BITS;

pub(crate) fn internal(predicate: u64) -> bool {
    predicate >= CHAIN
}

/// Predicate IDs stay stable while a ground program is extended.
#[derive(Clone, Default)]
pub(crate) struct Predicates(Option<Arc<HashMap<u64, u64>>>);

impl Predicates {
    fn index(&mut self, cell: u64) -> u64 {
        let cells = self.0.get_or_insert_with(Arc::default);
        if let Some(&index) = cells.get(&cell) {
            return index;
        }
        let next = cells.len() as u64;
        assert!(next < (1 << PAYLOAD_BITS));
        Arc::make_mut(cells).insert(cell, next);
        next
    }

    pub(crate) fn remap(&mut self, lost: u64, to: u64) -> Option<[(u64, u64); 2]> {
        let from = *self.0.as_ref()?.get(&lost)?;
        let to = self.index(to);
        Some([(CHAIN | from, CHAIN | to), (KEY | from, KEY | to)])
    }
}

/// The fixed-arity unfolding of an accepting cell graph. A rest to nil can
/// terminate here or continue, if nil is also an explicit accepting cell.
pub(super) fn compile(
    voc: &ListVocabulary,
    kind: ListKind,
    axiom @ [subject, _, head]: Triple,
    cells: &HashMap<u64, Cell>,
    predicates: &mut Predicates,
    mut emit: impl FnMut(Rule, Vec<Triple>),
) {
    let (x, y, z) = (v(0), v(1), v(2));
    let ty = c(voc.rdf_type);
    let membership = |term, class| Atom([term, ty, c(class)]);
    let mut part = |node, start, end| match kind {
        ListKind::Chain => Atom([start, c(CHAIN | predicates.index(node)), end]),
        ListKind::Key => Atom([start, c(KEY | predicates.index(node)), end]),
        ListKind::Intersection => Atom([start, c(INTERSECTION), c(node)]),
        _ => unreachable!("this list kind has no private relation"),
    };
    let name = match kind {
        ListKind::Chain => "prp-spo2",
        ListKind::Key => "prp-key",
        ListKind::Intersection => "cls-int1",
        _ => unreachable!("this list kind has no private relation"),
    };
    let (head_body, mut head_guards, conclusion) = match kind {
        ListKind::Chain => (vec![part(head, x, z)], vec![], Atom([x, c(subject), z])),
        ListKind::Intersection => (vec![part(head, x, z)], vec![], membership(x, subject)),
        _ => (
            vec![
                part(head, x, y),
                membership(x, subject),
                membership(y, subject),
            ],
            vec![Guard::NotEqual(x, y)],
            Atom([x, c(voc.same_as), y]),
        ),
    };
    if matches!(kind, ListKind::Key)
        && let Some((low, high)) = voc.named_keys
    {
        head_guards.extend([Guard::NotIn(x, low, high), Guard::NotIn(y, low, high)]);
    }
    emit(
        rule(name, head_body, head_guards, Head::Facts(vec![conclusion])),
        vec![axiom],
    );
    // The first/step premises also carry the axiom that licensed the cell.
    // A shared key tail gets one guard for each axiom's class that reaches it.
    for (&node, cell) in cells {
        for &member in &cell.firsts {
            if matches!(kind, ListKind::Key) && member == voc.same_as {
                continue;
            }
            for &rest in &cell.rests {
                let (body, guards, current, tail) = match kind {
                    ListKind::Chain => (
                        vec![Atom([x, c(member), y])],
                        vec![],
                        part(node, x, z),
                        part(rest, y, z),
                    ),
                    ListKind::Intersection => (
                        vec![membership(x, member)],
                        vec![],
                        part(node, x, z),
                        part(rest, x, z),
                    ),
                    _ => (
                        vec![
                            membership(x, subject),
                            membership(y, subject),
                            Atom([x, c(member), z]),
                            Atom([y, c(member), z]),
                        ],
                        voc.named_keys.map_or_else(Vec::new, |(low, high)| {
                            vec![
                                Guard::NotIn(x, low, high),
                                Guard::NotIn(y, low, high),
                                Guard::NotIn(z, low, high),
                            ]
                        }),
                        part(node, x, y),
                        part(rest, x, y),
                    ),
                };
                let used = || vec![axiom, [node, voc.first, member], [node, voc.rest, rest]];
                if rest == voc.nil {
                    let end = if matches!(kind, ListKind::Chain) {
                        part(node, x, y)
                    } else {
                        current
                    };
                    emit(
                        rule(name, body.clone(), guards.clone(), Head::Facts(vec![end])),
                        used(),
                    );
                    if matches!(kind, ListKind::Chain) && member == voc.same_as {
                        for rule in diagonal(end.0[1]) {
                            emit(rule, used());
                        }
                    }
                }
                if cells.get(&rest).is_some_and(|tail| !tail.rests.is_empty()) {
                    if matches!(kind, ListKind::Chain) && member == voc.same_as {
                        let reflexive = part(rest, x, z);
                        emit(
                            rule(name, vec![reflexive], vec![], Head::Facts(vec![current])),
                            used(),
                        );
                    }
                    let mut step = body;
                    step.push(tail);
                    emit(rule(name, step, guards, Head::Facts(vec![current])), used());
                }
            }
        }
    }
}
