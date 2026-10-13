//! `COUNT(*)` of a join tree without its rows: basic graph patterns whose variables form a
//! forest, with OPTIONAL, MINUS, (NOT) EXISTS and UNION parts that each share at most one
//! variable with what they hang off, counted by message passing over the index (variable
//! elimination in the sum-product semiring: Abo Khamis, Ngo and Rudra, "FAQ: Questions
//! Asked Frequently", PODS 2016; Olteanu and Závodný's factorised databases).
//!
//! Each variable's values carry a weight: the solutions of what hangs below it with the
//! variable bound to the value. A leaf sends its parent, per value of the parent, the
//! joining pattern's matches: run lengths of the index sorted on the parent (a group
//! walk). A variable with weights of its own sends, per value of the parent, the sum of its
//! weights over the values the joining pattern pairs with it. The count is the sum of the
//! root's weights; rooted at a pattern, the sum over the pattern's matches of the product of
//! its two ends' weights, the matches read forward from the end with fewer values and never
//! kept. Memory holds a weight per distinct value of a variable, never a row of the join:
//! as rows, Sparqloscope's query 4 (157.9 M) took 9.0 GB, its query 19 past 26 GB.
//!
//! A part hanging off `?k` multiplies `?k`'s weights: OPTIONAL by max(1, its solutions per
//! value), MINUS and NOT EXISTS by whether it has none, EXISTS by whether it has some; a
//! UNION's branches add up. A part sharing no variable is a factor of the whole count (a
//! MINUS sharing none removes nothing). Patterns over the same two variables are one edge,
//! their matches intersected per value (Sparqloscope's multicolumn joins). Anything else (a
//! cycle, a pattern with three variables or one twice, a part sharing two variables or one
//! that may be unbound, filters, paths, graph variables or merged graphs, equality classes
//! expanded) isn't counted here: the pattern is evaluated as before.

use nrese_engine::ProbeCursor;
use nrese_exec::Reservation;

use super::*;

/// Pairs a message collects before they are added up by value.
const COMPACT_PAIRS: usize = 1 << 20;
/// Values read between two checks of the query's cancellation.
const CHECK_EVERY: usize = 1 << 12;
/// Driving values per parallel chunk of a root pattern.
const DRIVE_CHUNK: usize = 1 << 14;

/// Relative costs per index entry, for choosing the root: a group walk reads run lengths,
/// a pattern read at the root streams its matches, a message by pairs reads, keeps and
/// sorts them.
const WALK_COST: f64 = 1.0;
const DRIVE_COST: f64 = 3.0;
const PAIRS_COST: f64 = 8.0;

/// Why a count stopped: the pattern isn't a tree this module counts, or the evaluation
/// failed (cancelled, out of memory).
enum Stop {
    Declined,
    Failed(NativeError),
}

impl From<NativeError> for Stop {
    fn from(error: NativeError) -> Self {
        Stop::Failed(error)
    }
}

type Counted<T> = Result<T, Stop>;

/// What a part does to the weights of the variable it hangs off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Optional,
    Minus,
    Exists,
    NotExists,
    Union,
}

/// A conjunction: triple patterns, and parts hanging off one of their variables or none.
struct Conj<'p> {
    atoms: Vec<&'p TriplePattern>,
    parts: Vec<Part<'p>>,
}

struct Part<'p> {
    kind: Kind,
    /// The variable it shares with the conjunction.
    at: Option<Variable>,
    /// Its pattern; a union's branches.
    branches: Vec<Conj<'p>>,
}

/// An element of a conjunction while it is read from the algebra.
enum Item<'p> {
    Atom(&'p TriplePattern),
    Part {
        kind: Kind,
        branches: Vec<&'p GraphPattern>,
        /// Where its left side's items start (OPTIONAL, MINUS, FILTER (NOT) EXISTS).
        left: usize,
    },
}

/// The items of `pattern` as one conjunction, in order; `false` where it has an operator
/// this module doesn't count through.
fn items<'p>(pattern: &'p GraphPattern, out: &mut Vec<Item<'p>>) -> bool {
    match pattern {
        GraphPattern::Bgp { patterns } => {
            out.extend(patterns.iter().map(Item::Atom));
            true
        }
        GraphPattern::Join { inputs } => inputs.iter().all(|input| items(input, out)),
        GraphPattern::LeftJoin {
            left,
            right,
            expression: None,
        } => {
            let start = out.len();
            if !items(left, out) {
                return false;
            }
            out.push(Item::Part {
                kind: Kind::Optional,
                branches: vec![right],
                left: start,
            });
            true
        }
        GraphPattern::Minus { left, right } => {
            let start = out.len();
            if !items(left, out) {
                return false;
            }
            out.push(Item::Part {
                kind: Kind::Minus,
                branches: vec![right],
                left: start,
            });
            true
        }
        GraphPattern::Filter { expr, inner } => {
            let start = out.len();
            if !items(inner, out) {
                return false;
            }
            let mut conjuncts = Vec::new();
            pushdown::conjuncts_of(expr, &mut conjuncts);
            for conjunct in conjuncts {
                let (kind, pattern) = match conjunct {
                    Expression::Exists(pattern) => (Kind::Exists, pattern),
                    Expression::Not(inner) => match &**inner {
                        Expression::Exists(pattern) => (Kind::NotExists, pattern),
                        _ => return false,
                    },
                    _ => return false,
                };
                out.push(Item::Part {
                    kind,
                    branches: vec![&**pattern],
                    left: start,
                });
            }
            true
        }
        GraphPattern::Union { inputs } => {
            out.push(Item::Part {
                kind: Kind::Union,
                branches: inputs.iter().collect(),
                left: out.len(),
            });
            true
        }
        _ => false,
    }
}

/// The variables of the triple patterns within `pattern` (blank nodes as variables), or
/// `false` where it holds an operator [`items`] refuses.
fn mentioned(pattern: &GraphPattern, out: &mut Vec<Variable>) -> bool {
    let mut found = Vec::new();
    if !items(pattern, &mut found) {
        return false;
    }
    for item in found {
        match item {
            Item::Atom(triple) => add_all(out, triple_variables(triple)),
            Item::Part { branches, .. } => {
                for branch in branches {
                    if !mentioned(branch, out) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// The variables a solution of `pattern` may bind (OPTIONAL's right side and UNION's
/// branches included, MINUS's and EXISTS's patterns not).
fn in_scope(pattern: &GraphPattern, out: &mut Vec<Variable>) -> bool {
    let mut found = Vec::new();
    if !items(pattern, &mut found) {
        return false;
    }
    for item in found {
        match item {
            Item::Atom(triple) => add_all(out, triple_variables(triple)),
            Item::Part { kind, branches, .. } => {
                if matches!(kind, Kind::Optional | Kind::Union) {
                    for branch in branches {
                        if !in_scope(branch, out) {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

fn add_all(out: &mut Vec<Variable>, variables: impl IntoIterator<Item = Variable>) {
    for v in variables {
        if !out.contains(&v) {
            out.push(v);
        }
    }
}

impl<'p> Conj<'p> {
    /// `pattern` as a conjunction with its parts, if it is one this module counts.
    fn of(pattern: &'p GraphPattern) -> Option<Self> {
        let mut found = Vec::new();
        if !items(pattern, &mut found) {
            return None;
        }
        // Per item: the variables its solutions bind for the rest of the conjunction
        // (MINUS's and EXISTS's only the one they share), and those certain to be bound.
        let mut visible: Vec<Vec<Variable>> = Vec::with_capacity(found.len());
        let mut shared: Vec<Option<Variable>> = Vec::with_capacity(found.len());
        for (i, item) in found.iter().enumerate() {
            match item {
                Item::Atom(triple) => {
                    visible.push(triple_variables(triple));
                    shared.push(None);
                }
                Item::Part {
                    kind,
                    branches,
                    left,
                } => {
                    // What its left side may and must bind.
                    let (mut scope, mut certain) = (Vec::new(), Vec::new());
                    for (j, earlier) in found[*left..i].iter().enumerate() {
                        add_all(&mut scope, visible[*left + j].iter().cloned());
                        if let Item::Atom(triple) = earlier {
                            add_all(&mut certain, triple_variables(triple));
                        }
                    }
                    let mut own = Vec::new();
                    for branch in branches {
                        let read = match kind {
                            Kind::Exists | Kind::NotExists => mentioned(branch, &mut own),
                            _ => in_scope(branch, &mut own),
                        };
                        if !read {
                            return None;
                        }
                    }
                    let at = match kind {
                        // A union joins the whole conjunction: settled below.
                        Kind::Union => None,
                        _ => {
                            let common: Vec<&Variable> =
                                own.iter().filter(|v| scope.contains(v)).collect();
                            match common.as_slice() {
                                [] => None,
                                [v] if certain.contains(v) => Some((*v).clone()),
                                _ => return None,
                            }
                        }
                    };
                    visible.push(match kind {
                        Kind::Optional | Kind::Union => own,
                        _ => at.iter().cloned().collect(),
                    });
                    shared.push(at);
                }
            }
        }
        // A union shares what the other items bind; an OPTIONAL's and a union's own
        // variables must meet nothing else (a join on a variable they may leave unbound).
        let certain: Vec<Variable> = found
            .iter()
            .filter_map(|item| match item {
                Item::Atom(triple) => Some(triple_variables(triple)),
                Item::Part { .. } => None,
            })
            .flatten()
            .collect();
        for (i, item) in found.iter().enumerate() {
            let Item::Part { kind, .. } = item else {
                continue;
            };
            if !matches!(kind, Kind::Optional | Kind::Union) {
                continue;
            }
            let others: Vec<&Variable> = visible
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .flat_map(|(_, vars)| vars)
                .collect();
            let met: Vec<&Variable> = visible[i].iter().filter(|v| others.contains(v)).collect();
            match (kind, met.as_slice()) {
                (_, []) => {}
                (Kind::Union, [v]) if certain.contains(v) => shared[i] = Some((*v).clone()),
                (Kind::Optional, [v]) if shared[i].as_ref() == Some(*v) => {}
                _ => return None,
            }
        }
        let mut conj = Conj {
            atoms: Vec::new(),
            parts: Vec::new(),
        };
        for (item, at) in found.into_iter().zip(shared) {
            match item {
                Item::Atom(triple) => conj.atoms.push(triple),
                Item::Part { kind, branches, .. } => {
                    let branches = branches
                        .into_iter()
                        .map(Conj::of)
                        .collect::<Option<Vec<_>>>()?;
                    conj.parts.push(Part { kind, at, branches });
                }
            }
        }
        Some(conj)
    }
}

/// A triple pattern with its constants resolved.
struct Atom {
    scan: ScanPattern,
    /// Its variables, each once, with the component (subject 0, predicate 1, object 2)
    /// each is in.
    vars: Vec<(Variable, usize)>,
    /// Its matches, for choosing the root.
    estimate: u64,
}

impl Atom {
    fn component(&self, variable: &Variable) -> usize {
        self.vars
            .iter()
            .find(|(v, _)| v == variable)
            .map(|&(_, c)| c)
            .expect("the atom's variable")
    }

    /// Its quad pattern with `variable` bound to `value`.
    fn bound(&self, variable: &Variable, value: u64) -> QuadPattern {
        let mut pattern = self.scan.quad_pattern();
        let id = Some(TermId::from_raw(value));
        match self.component(variable) {
            0 => pattern.subject = id,
            1 => pattern.predicate = id,
            _ => pattern.object = id,
        }
        pattern
    }
}

/// Weights per value of a variable, sorted by value and each value once, and the weight of
/// every value not listed: 0 where the variable must match something, 1 or more where
/// only parts that modify it (OPTIONAL, MINUS) were met. The entries' memory is charged
/// to the query's budget while they live.
struct Weights<'b> {
    entries: Vec<(u64, u64)>,
    default: u64,
    _held: Reservation<&'b Budget>,
}

impl<'b> Weights<'b> {
    fn new(budget: &'b Budget, entries: Vec<(u64, u64)>, default: u64) -> Counted<Self> {
        let mut held = Reservation::new(budget);
        held.charge(entries.capacity() * 16)
            .map_err(|e| Stop::Failed(QueryEvaluationError::MemoryLimit(e).into()))?;
        Ok(Self {
            entries,
            default,
            _held: held,
        })
    }

    /// The weight of `value`, the entries from `from` on searched (a forward walk).
    fn weight(&self, value: u64, from: &mut usize) -> u64 {
        let rest = &self.entries[*from..];
        let k = rest.partition_point(|&(v, _)| v < value);
        *from += k;
        match rest.get(k) {
            Some(&(v, w)) if v == value => w,
            _ => self.default,
        }
    }

    /// Their sum over every listed value (the default must be 0).
    fn total(&self) -> Counted<u64> {
        debug_assert_eq!(self.default, 0);
        self.entries
            .iter()
            .try_fold(0u64, |sum, &(_, w)| sum.checked_add(w))
            .ok_or(Stop::Declined)
    }
}

/// What a count did, for EXPLAIN and the guards.
#[derive(Default)]
struct Stats {
    /// Group walks (a pattern's matches per value from the index's run lengths).
    walks: usize,
    /// Weights listed, over every message.
    listed: usize,
    /// Pairs read and kept for a message from a variable with weights of its own.
    pairs: usize,
    /// Matches of root patterns read and not kept.
    driven: usize,
    /// The roots: a variable or a pattern per tree.
    roots: Vec<String>,
}

/// The sum `a + b` and product `a * b`, `Declined` past `u64`: the count path would then
/// overflow too, and the rows are left to the ordinary evaluation's limits.
fn add(a: u64, b: u64) -> Counted<u64> {
    a.checked_add(b).ok_or(Stop::Declined)
}

fn mul(a: u64, b: u64) -> Counted<u64> {
    a.checked_mul(b).ok_or(Stop::Declined)
}

/// Sorts `pairs` by value and adds the weights of each value.
fn combine(pairs: &mut Vec<(u64, u64)>) -> Counted<()> {
    pairs.sort_unstable_by_key(|&(v, _)| v);
    let mut overflow = false;
    pairs.dedup_by(|later, kept| {
        let same = later.0 == kept.0;
        if same {
            match kept.1.checked_add(later.1) {
                Some(sum) => kept.1 = sum,
                None => overflow = true,
            }
        }
        same
    });
    match overflow {
        true => Err(Stop::Declined),
        false => Ok(()),
    }
}

/// One tree of the variable graph: its variables and patterns.
struct Tree {
    vars: Vec<usize>,
    edges: Vec<usize>,
}

/// A root: a variable, or a pattern between two.
#[derive(Clone, Copy, Debug)]
enum Root {
    Var(usize),
    Edge(usize),
}

/// A conjunction's variable graph: its edges are the patterns over two variables, those
/// over the same two one edge (their matches intersected).
struct Graph<'c> {
    vars: Vec<Variable>,
    atoms: &'c [Atom],
    /// Per variable: its patterns with no other variable.
    unary: Vec<Vec<usize>>,
    /// Per edge: its patterns.
    edges: Vec<Vec<usize>>,
    /// Per variable: (neighbour, edge).
    adjacent: Vec<Vec<(usize, usize)>>,
    /// Per variable: whether a part hangs off it that only keeps values with solutions
    /// (EXISTS, UNION); OPTIONAL and MINUS keep every value.
    restricted: Vec<bool>,
    /// Per variable: whether a part hanging off it weighs some values apart (an OPTIONAL
    /// with more than one match, a MINUS that matches): its message then keeps their
    /// partners as pairs.
    modified: Vec<bool>,
}

impl Graph<'_> {
    fn index(&self, variable: &Variable) -> usize {
        self.vars
            .iter()
            .position(|v| v == variable)
            .expect("a variable of the graph")
    }

    /// Whether `var`'s weights, without what comes through `edge`, list values with 0 for
    /// the others: it has a pattern of its own, a restricting part, or another neighbour.
    fn listed(&self, var: usize, edge: Option<usize>) -> bool {
        !self.unary[var].is_empty()
            || self.restricted[var]
            || self.adjacent[var].iter().any(|&(_, e)| Some(e) != edge)
    }

    /// The trees of the graph, `None` where it has a cycle.
    fn trees(&self) -> Option<Vec<Tree>> {
        let n = self.vars.len();
        let mut seen = vec![false; n];
        let mut trees = Vec::new();
        for start in 0..n {
            if seen[start] {
                continue;
            }
            let mut tree = Tree {
                vars: vec![start],
                edges: Vec::new(),
            };
            seen[start] = true;
            let mut stack = vec![start];
            while let Some(x) = stack.pop() {
                for &(y, e) in &self.adjacent[x] {
                    if !tree.edges.contains(&e) {
                        tree.edges.push(e);
                    }
                    if !seen[y] {
                        seen[y] = true;
                        tree.vars.push(y);
                        stack.push(y);
                    }
                }
            }
            if tree.edges.len() + 1 != tree.vars.len() {
                return None;
            }
            trees.push(tree);
        }
        Some(trees)
    }

    /// The cheapest root of `tree` ([`WALK_COST`] and the others): each variable, and
    /// each pattern one of whose ends has values listed.
    fn root(&self, tree: &Tree) -> Root {
        let mut best = (f64::INFINITY, Root::Var(tree.vars[0]));
        for &var in &tree.vars {
            let cost = self.cost_below(var, None);
            if cost < best.0 {
                best = (cost, Root::Var(var));
            }
        }
        for &edge in &tree.edges {
            let (x, y) = self.ends(edge);
            if !self.listed(x, Some(edge)) && !self.listed(y, Some(edge)) {
                continue;
            }
            let cost = self.cost_below(x, Some(edge))
                + self.cost_below(y, Some(edge))
                + DRIVE_COST * self.estimate(edge);
            if cost < best.0 {
                best = (cost, Root::Edge(edge));
            }
        }
        best.1
    }

    /// The cost of the messages `var` receives from below, `edge` leading to its parent.
    fn cost_below(&self, var: usize, edge: Option<usize>) -> f64 {
        self.adjacent[var]
            .iter()
            .filter(|&&(_, e)| Some(e) != edge)
            .map(|&(child, e)| {
                let per_entry = match self.listed(child, Some(e)) || self.modified[child] {
                    true => PAIRS_COST,
                    false => WALK_COST,
                };
                per_entry * self.estimate(e) + self.cost_below(child, Some(e))
            })
            .sum()
    }

    /// An edge's matches at most: its smallest pattern's.
    fn estimate(&self, edge: usize) -> f64 {
        self.edges[edge]
            .iter()
            .map(|&a| self.atoms[a].estimate)
            .min()
            .unwrap_or(0) as f64
    }

    /// The two variables of an edge.
    fn ends(&self, edge: usize) -> (usize, usize) {
        let vars = &self.atoms[self.edges[edge][0]].vars;
        (self.index(&vars[0].0), self.index(&vars[1].0))
    }

    /// An edge's patterns.
    fn atoms_of(&self, edge: usize) -> Vec<&Atom> {
        self.edges[edge].iter().map(|&a| &self.atoms[a]).collect()
    }
}

/// The partners of a value through an edge: each pattern's values read by a cursor of its
/// own (so each reads its index forward), intersected.
struct Partners<'s> {
    cursors: Vec<ProbeCursor<'s>>,
    scratch: Vec<u64>,
}

impl<'s> Partners<'s> {
    fn new(reader: Reader<'s>, atoms: usize) -> Self {
        Self {
            cursors: (0..atoms)
                .map(|_| reader.snapshot.probe_cursor(reader.model))
                .collect(),
            scratch: Vec::new(),
        }
    }

    /// Into `out`, ascending: the values of `to` that every one of `atoms` pairs with
    /// `value` of `by`; `false` where the index can't list them.
    fn read(
        &mut self,
        atoms: &[&Atom],
        by: &Variable,
        value: u64,
        to: &Variable,
        out: &mut Vec<u64>,
    ) -> bool {
        out.clear();
        for (k, atom) in atoms.iter().enumerate() {
            let pattern = atom.bound(by, value);
            let component = atom.component(to);
            if k == 0 {
                if !self.cursors[0].values(&pattern, component, out) {
                    return false;
                }
            } else {
                self.scratch.clear();
                if !self.cursors[k].values(&pattern, component, &mut self.scratch) {
                    return false;
                }
                let mut rest = self.scratch.as_slice();
                out.retain(|&v| {
                    rest = &rest[rest.partition_point(|&w| w < v)..];
                    rest.first() == Some(&v)
                });
            }
            if out.is_empty() {
                break;
            }
        }
        true
    }

    /// How many values of `to` `atoms` pair with `value` of `by` (`buffer` holds them
    /// where there are several patterns to intersect).
    fn count(
        &mut self,
        atoms: &[&Atom],
        by: &Variable,
        value: u64,
        to: &Variable,
        buffer: &mut Vec<u64>,
    ) -> Option<u64> {
        if let [atom] = atoms {
            return Some(self.cursors[0].count(&atom.bound(by, value)));
        }
        self.read(atoms, by, value, to, buffer)
            .then_some(buffer.len() as u64)
    }
}

/// The shared parts of a count: the snapshot read and the query's cancellation.
#[derive(Clone, Copy)]
struct Reader<'s> {
    snapshot: &'s Snapshot,
    model: ReadModel,
    cancellation: Option<&'s CancellationToken>,
}

impl Reader<'_> {
    fn check(&self) -> Counted<()> {
        match self.cancellation {
            Some(token) => token.check().map_err(|e| Stop::Failed(e.into())),
            None => Ok(()),
        }
    }
}

impl Context<'_> {
    /// `aggregate` (`COUNT(*)`, or `COUNT(?v)` of a variable every solution binds) of
    /// `pattern` by message passing ([module docs](self)); `None` where it isn't a tree
    /// counted so.
    pub(super) fn count_tree(
        &self,
        pattern: &GraphPattern,
        aggregate: &AggregateExpression,
    ) -> NativeResult<Option<u64>> {
        if self.as_written || self.late.is_some() {
            return Ok(None);
        }
        // A single pattern has its count from the index.
        if let GraphPattern::Bgp { patterns } = pattern
            && patterns.len() < 2
        {
            return Ok(None);
        }
        let Some(conj) = Conj::of(pattern) else {
            return Ok(None);
        };
        let counted = match aggregate {
            AggregateExpression::CountSolutions { distinct: false } => true,
            AggregateExpression::FunctionCall {
                name: AggregateFunction::Count,
                expr: Expression::Variable(v),
                distinct: false,
            } => conj
                .atoms
                .iter()
                .any(|triple| triple_variables(triple).contains(v)),
            _ => false,
        };
        if !counted {
            return Ok(None);
        }
        let start = Instant::now();
        let mut stats = Stats::default();
        let count = match self.solve(&conj, None, &mut stats) {
            Ok(Solved::Count(n)) => n,
            Ok(Solved::Weights(_)) => unreachable!("a conjunction without a key counts"),
            Err(Stop::Declined) => return Ok(None),
            Err(Stop::Failed(error)) => return Err(error),
        };
        if self.trace.is_some() {
            let detail = format!(
                "rooted at {}; {} group walks, {} weights listed, {} pairs kept, {} matches read at the roots; no row made",
                stats.roots.join(", "),
                stats.walks,
                stats.listed,
                stats.pairs,
                stats.driven,
            );
            self.note("counted join", detail, Some(1), 1, start);
        }
        Ok(Some(count))
    }

    fn reader(&self) -> Reader<'_> {
        Reader {
            snapshot: &self.snapshot,
            model: self.model,
            cancellation: self.cancellation.as_ref(),
        }
    }

    /// The atom of `triple`; `Ok(None)` where it can't match (a constant the store doesn't
    /// know, a graph it doesn't have).
    fn count_atom(&self, triple: &TriplePattern, all: &[&TriplePattern]) -> Counted<Option<Atom>> {
        let triples: Vec<TriplePattern> = all.iter().map(|&t| t.clone()).collect();
        if search::is_search(triple, &triples)
            || self.is_spatial(triple)
            || matches!(triple.subject, TermPattern::Triple(_))
            || matches!(triple.object, TermPattern::Triple(_))
        {
            return Err(Stop::Declined);
        }
        let Some(scan) = self.scan_pattern(triple) else {
            return Ok(None);
        };
        if !matches!(scan.slots[3], Slot::Const(_)) || scan.repeats_variable() {
            return Err(Stop::Declined);
        }
        let vars: Vec<(Variable, usize)> = (0..3)
            .filter_map(|c| match &scan.slots[c] {
                Slot::Var(v) => Some((v.clone(), c)),
                _ => None,
            })
            .collect();
        if vars.len() > 2 {
            return Err(Stop::Declined);
        }
        let estimate = self.snapshot.estimate_in(self.model, &scan.quad_pattern());
        Ok(Some(Atom {
            scan,
            vars,
            estimate,
        }))
    }

    /// No solutions: none per value of a key, or 0.
    fn no_solutions(&self, keyed: bool) -> Counted<Solved<'_>> {
        Ok(match keyed {
            true => Solved::Weights(Weights::new(&self.budget, Vec::new(), 0)?),
            false => Solved::Count(0),
        })
    }

    /// The solutions of `conj`: their number, or with `key` their number per value of it.
    fn solve(
        &self,
        conj: &Conj<'_>,
        key: Option<&Variable>,
        stats: &mut Stats,
    ) -> Counted<Solved<'_>> {
        let mut atoms = Vec::with_capacity(conj.atoms.len());
        for triple in &conj.atoms {
            match self.count_atom(triple, &conj.atoms)? {
                Some(atom) => atoms.push(atom),
                None => return self.no_solutions(key.is_some()),
            }
        }
        // Patterns without variables: whether the statement is there.
        for atom in atoms.iter().filter(|a| a.vars.is_empty()) {
            if !self
                .snapshot
                .exists_in(self.model, &atom.scan.quad_pattern())
            {
                return self.no_solutions(key.is_some());
            }
        }
        let mut vars: Vec<Variable> = Vec::new();
        for atom in &atoms {
            add_all(&mut vars, atom.vars.iter().map(|(v, _)| v.clone()));
        }
        if let Some(key) = key
            && !vars.contains(key)
        {
            return Err(Stop::Declined);
        }
        let mut graph = Graph {
            unary: vec![Vec::new(); vars.len()],
            edges: Vec::new(),
            adjacent: vec![Vec::new(); vars.len()],
            restricted: vec![false; vars.len()],
            modified: vec![false; vars.len()],
            vars,
            atoms: &atoms,
        };
        for (i, atom) in atoms.iter().enumerate() {
            match atom.vars.as_slice() {
                [] => {}
                [(v, _)] => {
                    let v = graph.index(v);
                    graph.unary[v].push(i);
                }
                [(a, _), (b, _)] => {
                    let (a, b) = (graph.index(a), graph.index(b));
                    match graph.adjacent[a].iter().find(|&&(other, _)| other == b) {
                        Some(&(_, edge)) => graph.edges[edge].push(i),
                        None => {
                            let edge = graph.edges.len();
                            graph.edges.push(vec![i]);
                            graph.adjacent[a].push((b, edge));
                            graph.adjacent[b].push((a, edge));
                        }
                    }
                }
                _ => unreachable!("at most two variables"),
            }
        }
        // Parts sharing no variable are factors of the count; the others weigh their
        // variable's values.
        let mut scale = 1u64;
        let mut hanging: Vec<Vec<Weights<'_>>> =
            (0..graph.vars.len()).map(|_| Vec::new()).collect();
        for part in &conj.parts {
            match &part.at {
                None => {
                    scale = mul(scale, self.part_factor(part, stats)?)?;
                    if scale == 0 {
                        return self.no_solutions(key.is_some());
                    }
                }
                Some(at) => {
                    let var = graph.index(at);
                    let weights = self.part_weights(part, at, stats)?;
                    graph.restricted[var] |= weights.default == 0;
                    graph.modified[var] |= weights.default != 0 && !weights.entries.is_empty();
                    hanging[var].push(weights);
                }
            }
        }
        let Some(trees) = graph.trees() else {
            return Err(Stop::Declined);
        };
        let key = key.map(|k| graph.index(k));
        let mut keyed = None;
        for tree in &trees {
            let root = match key {
                Some(k) if tree.vars.contains(&k) => Root::Var(k),
                _ => graph.root(tree),
            };
            stats.roots.push(match root {
                Root::Var(v) => graph.vars[v].to_string(),
                Root::Edge(e) => {
                    let (x, y) = graph.ends(e);
                    format!("the pattern of {} and {}", graph.vars[x], graph.vars[y])
                }
            });
            match root {
                Root::Var(v) => {
                    let weights = self.weights_at(&graph, &mut hanging, v, None, stats)?;
                    if Some(v) == key {
                        keyed = Some(weights);
                    } else {
                        scale = mul(scale, weights.total()?)?;
                    }
                }
                Root::Edge(e) => {
                    let (x, y) = graph.ends(e);
                    let wx = self.weights_at(&graph, &mut hanging, x, Some(e), stats)?;
                    let wy = self.weights_at(&graph, &mut hanging, y, Some(e), stats)?;
                    let total = self.drive(
                        &graph.atoms_of(e),
                        &graph.vars[x],
                        wx,
                        &graph.vars[y],
                        wy,
                        stats,
                    )?;
                    scale = mul(scale, total)?;
                }
            }
            if scale == 0 {
                return self.no_solutions(key.is_some());
            }
        }
        match keyed {
            None => Ok(Solved::Count(scale)),
            Some(mut weights) => {
                if scale != 1 {
                    for entry in &mut weights.entries {
                        entry.1 = mul(entry.1, scale)?;
                    }
                }
                Ok(Solved::Weights(weights))
            }
        }
    }

    /// The factor of a part sharing no variable.
    fn part_factor(&self, part: &Part<'_>, stats: &mut Stats) -> Counted<u64> {
        if part.kind == Kind::Minus {
            return Ok(1);
        }
        let mut sum = 0u64;
        for branch in &part.branches {
            let Solved::Count(n) = self.solve(branch, None, stats)? else {
                unreachable!("no key, a count")
            };
            sum = add(sum, n)?;
        }
        Ok(match part.kind {
            Kind::Optional => sum.max(1),
            Kind::Exists => u64::from(sum > 0),
            Kind::NotExists => u64::from(sum == 0),
            Kind::Union => sum,
            Kind::Minus => unreachable!("handled above"),
        })
    }

    /// The weights a part hanging off `at` gives `at`'s values.
    fn part_weights(
        &self,
        part: &Part<'_>,
        at: &Variable,
        stats: &mut Stats,
    ) -> Counted<Weights<'_>> {
        let mut found: Option<Weights<'_>> = None;
        for branch in &part.branches {
            let Solved::Weights(weights) = self.solve(branch, Some(at), stats)? else {
                unreachable!("a key, weights")
            };
            found = Some(match found {
                None => weights,
                Some(before) => self.sum(before, weights)?,
            });
        }
        let mut weights = found.expect("a part has a branch");
        match part.kind {
            Kind::Union => {}
            // A value with one match weighs what one without does.
            Kind::Optional => {
                weights.entries.retain(|e| e.1 > 1);
                weights.default = 1;
            }
            Kind::Exists => weights.entries.iter_mut().for_each(|e| e.1 = 1),
            Kind::Minus | Kind::NotExists => {
                weights.entries.iter_mut().for_each(|e| e.1 = 0);
                weights.default = 1;
            }
        }
        stats.listed += weights.entries.len();
        Ok(weights)
    }

    /// The weights of `var`'s values: the product of its patterns' matches, the parts
    /// hanging off it and what each child sends, `edge` leading to its parent.
    fn weights_at<'b>(
        &'b self,
        graph: &Graph<'_>,
        hanging: &mut [Vec<Weights<'b>>],
        var: usize,
        edge: Option<usize>,
        stats: &mut Stats,
    ) -> Counted<Weights<'b>> {
        let mut factors: Vec<Weights<'b>> = std::mem::take(&mut hanging[var]);
        for &atom in &graph.unary[var] {
            let entries = self.walk(&graph.atoms[atom], &graph.vars[var], stats)?;
            factors.push(Weights::new(&self.budget, entries, 0)?);
        }
        for &(child, e) in &graph.adjacent[var] {
            if Some(e) == edge {
                continue;
            }
            let below = self.weights_at(graph, hanging, child, Some(e), stats)?;
            let atoms = graph.atoms_of(e);
            factors.push(self.message(
                &atoms,
                &graph.vars[child],
                &graph.vars[var],
                below,
                stats,
            )?);
        }
        // The fewest listed values first: a product keeps only values listed in each.
        factors.sort_by_key(|w| (w.default != 0, w.entries.len()));
        let mut factors = factors.into_iter();
        let mut product = match factors.next() {
            Some(first) => first,
            None => Weights::new(&self.budget, Vec::new(), 1)?,
        };
        for factor in factors {
            product = self.product(product, factor)?;
        }
        Ok(product)
    }

    /// `atom`'s matches per value of `var` (the first free component of an index order),
    /// from the index's run lengths.
    fn walk(&self, atom: &Atom, var: &Variable, stats: &mut Stats) -> Counted<Vec<(u64, u64)>> {
        let permutation = atom.scan.permutation_for(Some(var));
        if atom.scan.first_free(permutation) != Some(atom.component(var)) {
            return Err(Stop::Declined);
        }
        let groups = self
            .snapshot
            .group_counts_in(self.model, &atom.scan.quad_pattern(), permutation)
            .ok_or(Stop::Declined)?;
        self.check()?;
        stats.walks += 1;
        stats.listed += groups.len();
        Ok(groups.into_iter().map(|(id, n)| (id.raw(), n)).collect())
    }

    /// An edge's matches per value of `var`, `other` its other end: one pattern's from the
    /// index's run lengths; several patterns' by intersecting their partners per value of
    /// the one with the fewest matches.
    fn walk_edge(
        &self,
        atoms: &[&Atom],
        var: &Variable,
        other: &Variable,
        stats: &mut Stats,
    ) -> Counted<Vec<(u64, u64)>> {
        if let [atom] = atoms {
            return self.walk(atom, var, stats);
        }
        let first = atoms
            .iter()
            .min_by_key(|atom| atom.estimate)
            .expect("an edge has a pattern");
        let candidates = self.walk(first, var, stats)?;
        let reader = self.reader();
        let mut partners = Partners::new(reader, atoms.len());
        let (mut buffer, mut out) = (Vec::new(), Vec::new());
        for (n, &(value, _)) in candidates.iter().enumerate() {
            if n % CHECK_EVERY == 0 {
                reader.check()?;
            }
            let count = partners
                .count(atoms, var, value, other, &mut buffer)
                .ok_or(Stop::Declined)?;
            stats.pairs += count as usize;
            if count > 0 {
                out.push((value, count));
            }
        }
        Ok(out)
    }

    /// What `child` sends `parent` through the edge of `atoms`: per value of `parent`, the
    /// sum of `child`'s weights over the values the edge pairs with it.
    fn message<'b>(
        &'b self,
        atoms: &[&Atom],
        child: &Variable,
        parent: &Variable,
        below: Weights<'b>,
        stats: &mut Stats,
    ) -> Counted<Weights<'b>> {
        if below.entries.is_empty() {
            if below.default == 0 {
                return Weights::new(&self.budget, Vec::new(), 0);
            }
            // Every value of the child weighs the same: the matches per value, scaled.
            let mut entries = self.walk_edge(atoms, parent, child, stats)?;
            if below.default != 1 {
                for entry in &mut entries {
                    entry.1 = mul(entry.1, below.default)?;
                }
            }
            return Weights::new(&self.budget, entries, 0);
        }
        // The listed values' partners, weighed; for a default above 0 also the matches of
        // every value at the default, less what the listed values' matches had of it.
        let reader = self.reader();
        let mut partners = Partners::new(reader, atoms.len());
        let mut held = Reservation::new(&*self.budget);
        let mut pairs: Vec<(u64, u64)> = Vec::new();
        let mut less: Vec<(u64, u64)> = Vec::new();
        let mut values = Vec::new();
        let mut compacted = 0;
        for (n, &(value, weight)) in below.entries.iter().enumerate() {
            if n % CHECK_EVERY == 0 {
                reader.check()?;
            }
            if !partners.read(atoms, child, value, parent, &mut values) {
                return Err(Stop::Declined);
            }
            stats.pairs += values.len();
            if weight != 0 {
                pairs.extend(values.iter().map(|&v| (v, weight)));
            }
            if below.default != 0 {
                less.extend(values.iter().map(|&v| (v, below.default)));
            }
            if pairs.len() + less.len() >= COMPACT_PAIRS.max(2 * compacted) {
                combine(&mut pairs)?;
                combine(&mut less)?;
                compacted = pairs.len() + less.len();
            }
            let bytes = (pairs.capacity() + less.capacity() + values.capacity()) * 16;
            if bytes > held.bytes() {
                held.charge(bytes - held.bytes())
                    .map_err(|e| Stop::Failed(QueryEvaluationError::MemoryLimit(e).into()))?;
            }
        }
        combine(&mut pairs)?;
        let default = below.default;
        drop(below);
        if default == 0 {
            drop(held);
            return Weights::new(&self.budget, pairs, 0);
        }
        combine(&mut less)?;
        // Every value at the default: d times its matches, less d per listed partner
        // (`less`), plus the listed partners' weights (`pairs`).
        let mut entries = self.walk_edge(atoms, parent, child, stats)?;
        let (mut p, mut l) = (0, 0);
        for entry in &mut entries {
            let mut weight = mul(entry.1, default)?;
            l += less[l..].partition_point(|&(v, _)| v < entry.0);
            if let Some(&(v, d)) = less.get(l)
                && v == entry.0
            {
                weight -= d;
            }
            p += pairs[p..].partition_point(|&(v, _)| v < entry.0);
            if let Some(&(v, w)) = pairs.get(p)
                && v == entry.0
            {
                weight = add(weight, w)?;
            }
            entry.1 = weight;
        }
        entries.retain(|&(_, w)| w > 0);
        drop((pairs, less, held));
        Weights::new(&self.budget, entries, 0)
    }

    /// The product of two variables' weights: a value listed in one only takes the
    /// other's default.
    fn product<'b>(&'b self, a: Weights<'b>, b: Weights<'b>) -> Counted<Weights<'b>> {
        let default = mul(a.default, b.default)?;
        let mut entries = Vec::new();
        match (a.default, b.default) {
            // Only values listed in both: the shorter list looks its values up in the
            // longer, forward.
            (0, 0) => {
                let (short, long) = match a.entries.len() <= b.entries.len() {
                    true => (&a, &b),
                    false => (&b, &a),
                };
                let mut from = 0;
                for &(value, w) in &short.entries {
                    let other = long.weight(value, &mut from);
                    if other != 0 {
                        entries.push((value, mul(w, other)?));
                    }
                }
            }
            // Only the listed values of the one with default 0.
            (0, _) | (_, 0) => {
                let (listed, other) = match a.default == 0 {
                    true => (&a, &b),
                    false => (&b, &a),
                };
                let mut from = 0;
                for &(value, w) in &listed.entries {
                    let weight = mul(w, other.weight(value, &mut from))?;
                    if weight != 0 {
                        entries.push((value, weight));
                    }
                }
            }
            // Every value listed in either.
            _ => {
                let (mut i, mut j) = (0, 0);
                loop {
                    let (value, wa, wb) = match (a.entries.get(i), b.entries.get(j)) {
                        (Some(&(va, wa)), Some(&(vb, wb))) => match va.cmp(&vb) {
                            std::cmp::Ordering::Less => {
                                i += 1;
                                (va, wa, b.default)
                            }
                            std::cmp::Ordering::Greater => {
                                j += 1;
                                (vb, a.default, wb)
                            }
                            std::cmp::Ordering::Equal => {
                                i += 1;
                                j += 1;
                                (va, wa, wb)
                            }
                        },
                        (Some(&(va, wa)), None) => {
                            i += 1;
                            (va, wa, b.default)
                        }
                        (None, Some(&(vb, wb))) => {
                            j += 1;
                            (vb, a.default, wb)
                        }
                        (None, None) => break,
                    };
                    let weight = mul(wa, wb)?;
                    if weight != default {
                        entries.push((value, weight));
                    }
                }
            }
        }
        drop((a, b));
        Weights::new(&self.budget, entries, default)
    }

    /// The sum of two branches' weights per value (each lists every value it has).
    fn sum<'b>(&'b self, a: Weights<'b>, b: Weights<'b>) -> Counted<Weights<'b>> {
        debug_assert!(a.default == 0 && b.default == 0);
        let mut entries = Vec::with_capacity(a.entries.len() + b.entries.len());
        entries.extend_from_slice(&a.entries);
        entries.extend_from_slice(&b.entries);
        drop((a, b));
        combine(&mut entries)?;
        Weights::new(&self.budget, entries, 0)
    }

    /// The sum over the edge's matches of the product of its ends' weights: the partners
    /// of each listed value of the end with fewer of them read in turn, in parallel chunks,
    /// and none kept.
    fn drive(
        &self,
        atoms: &[&Atom],
        x: &Variable,
        wx: Weights<'_>,
        y: &Variable,
        wy: Weights<'_>,
        stats: &mut Stats,
    ) -> Counted<u64> {
        let x_drives = match (wx.default, wy.default) {
            (0, 0) => wx.entries.len() <= wy.entries.len(),
            (0, _) => true,
            (_, 0) => false,
            // Neither end lists its values: a root is never chosen so.
            _ => return Err(Stop::Declined),
        };
        let (driver, by, other, to) = match x_drives {
            true => (&wx, x, &wy, y),
            false => (&wy, y, &wx, x),
        };
        let reader = self.reader();
        let parts: Vec<Result<(u64, usize), bool>> = driver
            .entries
            .par_chunks(DRIVE_CHUNK)
            .map(|chunk| drive_chunk(reader, atoms, by, to, chunk, other))
            .collect();
        reader.check()?;
        let mut total = 0u64;
        for part in parts {
            match part {
                Ok((sum, read)) => {
                    total = add(total, sum)?;
                    stats.driven += read;
                }
                Err(_) => return Err(Stop::Declined),
            }
        }
        Ok(total)
    }
}

/// [`Context::drive`] over one chunk of the driving values: their weighted sum and the
/// matches read; `Err(true)` if the query was cancelled, `Err(false)` past `u64` or where
/// the index can't list the partners.
fn drive_chunk(
    reader: Reader<'_>,
    atoms: &[&Atom],
    by: &Variable,
    to: &Variable,
    chunk: &[(u64, u64)],
    other: &Weights<'_>,
) -> Result<(u64, usize), bool> {
    let mut partners = Partners::new(reader, atoms.len());
    let mut values = Vec::new();
    let (mut sum, mut read) = (0u64, 0usize);
    for (n, &(value, weight)) in chunk.iter().enumerate() {
        if n % CHECK_EVERY == 0
            && reader
                .cancellation
                .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(true);
        }
        if weight == 0 {
            continue;
        }
        let found = if other.entries.is_empty() {
            let n = partners
                .count(atoms, by, value, to, &mut values)
                .ok_or(false)?;
            read += n as usize;
            n.checked_mul(other.default).ok_or(false)?
        } else {
            if !partners.read(atoms, by, value, to, &mut values) {
                return Err(false);
            }
            read += values.len();
            let mut from = 0;
            values
                .iter()
                .try_fold(0u64, |s, &v| s.checked_add(other.weight(v, &mut from)))
                .ok_or(false)?
        };
        sum = weight
            .checked_mul(found)
            .and_then(|p| sum.checked_add(p))
            .ok_or(false)?;
    }
    Ok((sum, read))
}

/// What [`Context::solve`] gives: a count, or weights per value of the key.
enum Solved<'b> {
    Count(u64),
    Weights(Weights<'b>),
}
