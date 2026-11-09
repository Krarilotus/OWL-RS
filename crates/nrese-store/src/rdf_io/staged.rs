//! A document's statements staged for a write ([`StagedQuads`]): read once, kept
//! request-local and charged before they are allocated, written only once the whole document
//! has been read (UPLOAD option C; Harness notes/2026-11-09-upload-staging-design.md).

use std::hash::BuildHasher;
use std::sync::Arc;

use hashbrown::{DefaultHashBuilder, HashTable};
use nrese_engine::{EncodedQuad, TermId, Transaction};
use nrese_exec::{Budget, BudgetExceeded, ChargedMap, ChargedVec, Envelope, Reservation};
use nrese_rdf::{
    BaseDirection, BlankNodeRef, GraphName, GraphNameRef, LiteralRef, NamedNodeRef, QuadRef,
    TermRef, Triple,
};

use super::{owned_term_bytes, stored_graph_ref};
use crate::error::{StoreError, StoreResult};

/// No term: a statement's graph when it is the default graph, a language-tagged string's
/// datatype.
const NONE: u32 = u32::MAX;

/// A document's statements, each term kept once as text in one arena and each distinct
/// statement once as four local ids, all charged to a part of the request's envelope before it
/// is allocated. Nothing reaches the engine until [`Self::write`]: a document that fails to
/// parse, or whose staging is refused, leaves the dictionary and the transaction as they were.
/// Dropped, the buffers are freed before their charges are released (fields drop in order).
pub(crate) struct StagedQuads {
    /// The distinct terms, by local id: before `table_held`, which pays for it. Its keys are
    /// borrowed from the arena, so it isn't a [`ChargedMap`], which owns them.
    table: HashTable<u32>,
    /// The statements staged, so each is kept once.
    distinct: ChargedMap<[u32; 4], ()>,
    terms: ChargedVec<Term<Span>>,
    text: ChargedVec<u8>,
    /// Triple terms, owned: before `triples_held`, which pays for what they own.
    triples: ChargedVec<Triple>,
    statements: ChargedVec<[u32; 4]>,
    hasher: DefaultHashBuilder,
    budget: Arc<Budget>,
    /// Exactly `table`'s allocation, between growths.
    table_held: Reservation,
    triples_held: Reservation,
}

/// Text in the arena.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

/// A term as staged (`S`: spans of the arena) or as read (`S`: its text); two read terms are
/// one RDF term exactly when they are equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Term<S> {
    Named(S),
    Blank(S),
    /// A literal's datatype: kept once however many literals have it, never interned itself.
    Datatype(S),
    Literal {
        value: S,
        datatype: u32,
        language: Option<(S, Option<BaseDirection>)>,
    },
    /// A triple term, by its index in `triples`: rare, so kept owned, each occurrence apart.
    Triple(u32),
}

impl<S> Term<S> {
    fn try_map<T, E>(self, mut f: impl FnMut(S) -> Result<T, E>) -> Result<Term<T>, E> {
        Ok(match self {
            Self::Named(s) => Term::Named(f(s)?),
            Self::Blank(s) => Term::Blank(f(s)?),
            Self::Datatype(s) => Term::Datatype(f(s)?),
            Self::Literal {
                value,
                datatype,
                language,
            } => Term::Literal {
                value: f(value)?,
                datatype,
                language: match language {
                    Some((tag, direction)) => Some((f(tag)?, direction)),
                    None => None,
                },
            },
            Self::Triple(index) => Term::Triple(index),
        })
    }
}

fn text(arena: &[u8], span: Span) -> &str {
    std::str::from_utf8(&arena[span.start..span.end]).expect("the arena holds whole strings")
}

fn read(arena: &[u8], term: Term<Span>) -> Term<&str> {
    term.try_map(|span| Ok::<_, std::convert::Infallible>(text(arena, span)))
        .unwrap_or_else(|never| match never {})
}

/// Room for at least one more entry in `table`, doubling: the new table's bound charged to
/// `held` beside the old table's charge (a rehash holds both), the table grown, then the old
/// charge released and the new trimmed to the allocation ([`ChargedMap`]'s rule). `held` is
/// exactly the table's allocation before and after.
fn grow(
    table: &mut HashTable<u32>,
    held: &mut Reservation,
    hash: impl Fn(&u32) -> u64,
) -> Result<(), BudgetExceeded> {
    let to = (table.len() + 1).max(table.capacity().saturating_mul(2));
    let bound = nrese_exec::charged_map::table_bound::<u32>(to).unwrap_or(usize::MAX);
    held.charge(bound)?;
    let old = table.allocation_size();
    table.reserve(to - table.len(), hash);
    let allocated = table.allocation_size();
    assert!(
        allocated <= bound,
        "the table's allocation, {allocated} bytes, past its bound of {bound}"
    );
    held.release(old + (bound - allocated));
    Ok(())
}

impl StagedQuads {
    /// Empty, charging a part of `memory`.
    pub(crate) fn new(memory: &Envelope) -> Self {
        let budget = Arc::new(memory.part());
        Self {
            table: HashTable::new(),
            distinct: ChargedMap::new(Arc::clone(&budget)),
            terms: ChargedVec::new(Arc::clone(&budget)),
            text: ChargedVec::new(Arc::clone(&budget)),
            triples: ChargedVec::new(Arc::clone(&budget)),
            statements: ChargedVec::new(Arc::clone(&budget)),
            hasher: DefaultHashBuilder::default(),
            table_held: Reservation::new(Arc::clone(&budget)),
            triples_held: Reservation::new(Arc::clone(&budget)),
            budget,
        }
    }

    /// Stages `quad`, once however often it is read; with `graphs` false, without its graph
    /// (the write's contexts replace it, so it is neither kept nor interned).
    pub(crate) fn stage(&mut self, quad: QuadRef<'_>, graphs: bool) -> StoreResult<()> {
        let subject = self.term(quad.subject.into())?;
        let predicate = self.term(quad.predicate.into())?;
        let object = self.term(quad.object)?;
        let graph = match quad.graph_name {
            _ if !graphs => NONE,
            GraphNameRef::DefaultGraph => NONE,
            GraphNameRef::NamedNode(node) => self.term(node.into())?,
            GraphNameRef::BlankNode(node) => self.term(node.into())?,
        };
        let statement = [subject, predicate, object, graph];
        if self.distinct.get_or_insert_with(statement, || ())?.1 {
            self.statements.push(statement)?;
        }
        Ok(())
    }

    /// The local id of `term`, staged first if it is new.
    fn term(&mut self, term: TermRef<'_>) -> StoreResult<u32> {
        match term {
            TermRef::NamedNode(node) => self.id(Term::Named(node.as_str())),
            TermRef::BlankNode(node) => self.id(Term::Blank(node.as_str())),
            TermRef::Literal(literal) => {
                let language = literal.language().map(|tag| (tag, literal.direction()));
                // A language-tagged string's datatype follows from its tag: not kept.
                let datatype = match language {
                    Some(_) => NONE,
                    None => self.id(Term::Datatype(literal.datatype().as_str()))?,
                };
                self.id(Term::Literal {
                    value: literal.value(),
                    datatype,
                    language,
                })
            }
            TermRef::Triple(triple) => {
                let index = u32::try_from(self.triples.len()).expect("fewer triples than terms");
                self.triples_held.charge(owned_term_bytes(term))?;
                self.triples.push(triple.clone())?;
                self.push(Term::Triple(index))
            }
        }
    }

    /// The local id of `key`: its entry, or a new one, the table's growth, the text and the
    /// entry charged before each is allocated.
    fn id(&mut self, key: Term<&str>) -> StoreResult<u32> {
        let hash = self.hasher.hash_one(key);
        let (terms, arena, hasher) = (&self.terms, &self.text, &self.hasher);
        let same = |&id: &u32| read(arena, terms[id as usize]) == key;
        if let Some(&id) = self.table.find(hash, same) {
            return Ok(id);
        }
        if self.table.len() == self.table.capacity() {
            grow(&mut self.table, &mut self.table_held, |&id| {
                hasher.hash_one(read(arena, terms[id as usize]))
            })?;
        }
        let staged = key.try_map(|text| {
            let start = self.text.len();
            self.text.extend_from_slice(text.as_bytes())?;
            Ok::<_, BudgetExceeded>(Span {
                start,
                end: self.text.len(),
            })
        })?;
        let id = self.push(staged)?;
        let (terms, arena, hasher) = (&self.terms, &self.text, &self.hasher);
        self.table.insert_unique(hash, id, |&id| {
            hasher.hash_one(read(arena, terms[id as usize]))
        });
        Ok(id)
    }

    /// `term` as the next local id.
    fn push(&mut self, term: Term<Span>) -> StoreResult<u32> {
        let id = u32::try_from(self.terms.len())
            .ok()
            .filter(|&id| id != NONE)
            .ok_or_else(|| {
                StoreError::Configuration(
                    "a document with 2^32 - 1 distinct terms is too large to write in one request"
                        .to_owned(),
                )
            })?;
        self.terms.push(term)?;
        Ok(id)
    }

    /// The staged term `id`, borrowed.
    fn term_ref(&self, id: u32) -> TermRef<'_> {
        match read(&self.text, self.terms[id as usize]) {
            Term::Named(name) => NamedNodeRef::new_unchecked(name).into(),
            Term::Blank(label) => BlankNodeRef::new_unchecked(label).into(),
            Term::Literal {
                value,
                language: Some((tag, None)),
                ..
            } => LiteralRef::new_language_tagged_literal_unchecked(value, tag).into(),
            Term::Literal {
                value,
                language: Some((tag, Some(direction))),
                ..
            } => {
                LiteralRef::new_directional_language_tagged_literal_unchecked(value, tag, direction)
                    .into()
            }
            Term::Literal {
                value, datatype, ..
            } => match read(&self.text, self.terms[datatype as usize]) {
                Term::Datatype(iri) => {
                    LiteralRef::new_typed_literal(value, NamedNodeRef::new_unchecked(iri)).into()
                }
                _ => unreachable!("a literal's datatype is staged as one"),
            },
            Term::Triple(index) => TermRef::Triple(&self.triples[index as usize]),
            Term::Datatype(_) => unreachable!("a datatype is no statement's term"),
        }
    }

    /// The graph staged as `id`, as the store places it (`stored_graph_ref`).
    fn graph(&self, id: u32) -> GraphNameRef<'_> {
        stored_graph_ref(match id {
            NONE => GraphNameRef::DefaultGraph,
            id => match self.term_ref(id) {
                TermRef::NamedNode(node) => GraphNameRef::NamedNode(node),
                TermRef::BlankNode(node) => GraphNameRef::BlankNode(node),
                _ => unreachable!("a graph name is an IRI or a blank node"),
            },
        })
    }

    /// Writes the staged statements to `tx`, into `contexts` if any are given (each placed as
    /// the store places it), else into their own graphs; `check` is asked about every graph
    /// first. In this order, so that nothing is interned unless the whole write goes through:
    /// every graph checked; the pending room for every statement in every graph reserved
    /// where the transaction charges its pending changes; each distinct term interned once;
    /// then each statement inserted. Returns whether there was any statement.
    pub(crate) fn write(
        self,
        tx: &mut Transaction<'_>,
        contexts: &[GraphName],
        check: &mut dyn FnMut(GraphNameRef<'_>) -> StoreResult<()>,
    ) -> StoreResult<bool> {
        let contexts: Vec<GraphNameRef<'_>> = contexts
            .iter()
            .map(|graph| stored_graph_ref(graph.as_ref()))
            .collect();
        if contexts.is_empty() {
            let mut last = None;
            for &[.., graph] in self.statements.iter() {
                if last != Some(graph) {
                    check(self.graph(graph))?;
                    last = Some(graph);
                }
            }
        } else {
            contexts.iter().try_for_each(|&graph| check(graph))?;
        }
        if tx.charges_pending() {
            // Statements are charged 16 bytes each and contexts are a request's own list: a
            // product past the address space can't be staged.
            tx.try_reserve_inserts(self.statements.len().saturating_mul(contexts.len().max(1)))?;
        }
        let mut ids: ChargedVec<TermId> = ChargedVec::new(Arc::clone(&self.budget));
        ids.reserve_exact(self.terms.len())?;
        for (id, term) in self.terms.iter().enumerate() {
            ids.push(match term {
                // Never a term of its own (`TermId::DEFAULT_GRAPH` stands in, unread).
                Term::Datatype(_) => TermId::DEFAULT_GRAPH,
                _ => tx.intern(self.term_ref(id as u32)),
            })?;
        }
        let graphs: Vec<TermId> = contexts
            .iter()
            .map(|&graph| match graph {
                GraphNameRef::DefaultGraph => TermId::DEFAULT_GRAPH,
                GraphNameRef::NamedNode(node) => tx.intern(node.into()),
                GraphNameRef::BlankNode(node) => tx.intern(node.into()),
            })
            .collect();
        for &[subject, predicate, object, graph] in self.statements.iter() {
            let id = |local: u32| ids[local as usize];
            let quad = |graph| EncodedQuad::new(id(subject), id(predicate), id(object), graph);
            if graphs.is_empty() {
                // A staged graph the store keeps in the default graph is that graph.
                tx.try_insert_encoded(quad(match self.graph(graph) {
                    GraphNameRef::DefaultGraph => TermId::DEFAULT_GRAPH,
                    _ => id(graph),
                }))?;
            } else {
                for &graph in &graphs {
                    tx.try_insert_encoded(quad(graph))?;
                }
            }
        }
        Ok(!self.statements.is_empty())
    }
}
