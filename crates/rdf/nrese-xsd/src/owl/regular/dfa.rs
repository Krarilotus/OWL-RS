//! Deterministic automata over `Char`: built from a pattern (Thompson's construction, then
//! subsets), minimised, combined (product), complemented (they are complete), counted by
//! length and enumerated.
//!
//! The alphabet is a partition of `Char` into *symbols*: sets of characters every
//! transition treats alike, as sorted ranges; a symbol's weight is its number of
//! characters, so counts are of strings, not symbol words. State 0 is the start.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::chars::CharSet;
use super::syntax::{Ast, PatternError};
use crate::owl::line::{Line, integer_range};
use crate::owl::set::Count;

/// More states than this, built or determinised, and a pattern is refused.
const MAX_STATES: usize = 1 << 16;

/// Past this length the counts start from "at least one string to each reachable state"
/// rather than exact counts.
const EXACT_UPTO: i128 = 1024;

/// How many lengths past a segment's start are counted exactly before one that goes on
/// is "at least that many".
const WINDOW: i128 = 64;

#[derive(Debug)]
pub(crate) struct Dfa {
    /// `(from, to, symbol)`, sorted, covering `Char` exactly.
    ranges: Vec<(u32, u32, u32)>,
    /// Characters per symbol.
    sizes: Vec<u64>,
    symbols: usize,
    /// `next[state * symbols + symbol]`.
    next: Vec<u32>,
    accept: Vec<bool>,
    profile: OnceLock<Profile>,
}

/// What counting needs: the states on a path from the start to an acceptance.
#[derive(Debug)]
struct Profile {
    useful: Vec<bool>,
    useful_count: usize,
    /// Whether a cycle runs through useful states: infinitely many strings.
    cyclic: bool,
}

impl Clone for Dfa {
    fn clone(&self) -> Self {
        Self {
            ranges: self.ranges.clone(),
            sizes: self.sizes.clone(),
            symbols: self.symbols,
            next: self.next.clone(),
            accept: self.accept.clone(),
            profile: OnceLock::new(),
        }
    }
}

impl Dfa {
    /// Every string of `Char`s (or none).
    pub(crate) fn universal(accept: bool) -> Self {
        Self {
            ranges: CharSet::all()
                .ranges()
                .iter()
                .map(|&(a, b)| (a, b, 0))
                .collect(),
            sizes: vec![CharSet::all().len()],
            symbols: 1,
            next: vec![0],
            accept: vec![accept],
            profile: OnceLock::new(),
        }
    }

    pub(crate) fn from_ast(ast: &Ast) -> Result<Self, PatternError> {
        let mut nfa = Nfa::default();
        let (start, end) = nfa.build(ast)?;
        Ok(nfa.determinise(start, end)?.minimised())
    }

    fn states(&self) -> usize {
        self.accept.len()
    }

    fn symbol(&self, c: char) -> Option<usize> {
        let c = u32::from(c);
        let i = self.ranges.partition_point(|&(_, hi, _)| hi < c);
        self.ranges
            .get(i)
            .filter(|&&(lo, _, _)| lo <= c)
            .map(|&(_, _, s)| s as usize)
    }

    pub(crate) fn matches(&self, text: &str) -> bool {
        let mut q = 0usize;
        for c in text.chars() {
            let Some(s) = self.symbol(c) else {
                return false;
            };
            q = self.next[q * self.symbols + s] as usize;
        }
        self.accept[q]
    }

    pub(crate) fn complement(&self) -> Self {
        let mut out = self.clone();
        for a in &mut out.accept {
            *a = !*a;
        }
        out
    }

    pub(crate) fn intersection(&self, o: &Self) -> Self {
        self.product(o, |a, b| a && b)
    }

    /// The product automaton, accepting where `op` of the two acceptances holds.
    fn product(&self, o: &Self, op: impl Fn(bool, bool) -> bool) -> Self {
        // The joint partition: each range cut where either side cuts it.
        let mut pairs: HashMap<(u32, u32), u32> = HashMap::new();
        let mut symbol_pairs: Vec<(u32, u32)> = Vec::new();
        let mut sizes: Vec<u64> = Vec::new();
        let mut ranges = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < self.ranges.len() && j < o.ranges.len() {
            let (a, b, s) = self.ranges[i];
            let (c, d, t) = o.ranges[j];
            let (lo, hi) = (a.max(c), b.min(d));
            if lo <= hi {
                let id = *pairs.entry((s, t)).or_insert_with(|| {
                    symbol_pairs.push((s, t));
                    sizes.push(0);
                    (symbol_pairs.len() - 1) as u32
                });
                sizes[id as usize] += u64::from(hi - lo) + 1;
                push_range(&mut ranges, lo, hi, id);
            }
            if b < d {
                i += 1;
            } else {
                j += 1;
            }
        }
        let symbols = symbol_pairs.len();
        let mut ids: HashMap<(u32, u32), u32> = HashMap::from([((0, 0), 0)]);
        let mut queue = vec![(0u32, 0u32)];
        let mut next = Vec::new();
        let mut accept = Vec::new();
        let mut k = 0;
        while k < queue.len() {
            let (p, q) = queue[k];
            k += 1;
            accept.push(op(self.accept[p as usize], o.accept[q as usize]));
            for &(s, t) in &symbol_pairs {
                let target = (
                    self.next[p as usize * self.symbols + s as usize],
                    o.next[q as usize * o.symbols + t as usize],
                );
                let id = *ids.entry(target).or_insert_with(|| {
                    queue.push(target);
                    (queue.len() - 1) as u32
                });
                next.push(id);
            }
        }
        Self {
            ranges,
            sizes,
            symbols,
            next,
            accept,
            profile: OnceLock::new(),
        }
        .minimised()
    }

    /// The minimal automaton (Moore's refinement), with equivalent symbols merged.
    fn minimised(self) -> Self {
        let n = self.states();
        let mut class: Vec<u32> = self.accept.iter().map(|&a| u32::from(a)).collect();
        let mut classes = 0;
        loop {
            let rows: Vec<Vec<u32>> = (0..n)
                .map(|q| {
                    self.next[q * self.symbols..(q + 1) * self.symbols]
                        .iter()
                        .map(|&t| class[t as usize])
                        .collect()
                })
                .collect();
            let mut ids: HashMap<(u32, &[u32]), u32> = HashMap::new();
            let mut refined = Vec::with_capacity(n);
            for q in 0..n {
                let len = ids.len() as u32;
                refined.push(*ids.entry((class[q], &rows[q])).or_insert(len));
            }
            let count = ids.len();
            class = refined;
            if count == classes {
                break;
            }
            classes = count;
        }
        // Renumber so that the start's class is 0, in order of first appearance from it.
        let mut order: Vec<Option<u32>> = vec![None; classes];
        let mut representative = Vec::new();
        let mut queue = vec![0usize];
        order[class[0] as usize] = Some(0);
        representative.push(0usize);
        let mut k = 0;
        while k < queue.len() {
            let q = queue[k];
            k += 1;
            for s in 0..self.symbols {
                let t = self.next[q * self.symbols + s] as usize;
                if order[class[t] as usize].is_none() {
                    order[class[t] as usize] = Some(representative.len() as u32);
                    representative.push(t);
                    queue.push(t);
                }
            }
        }
        let states = representative.len();
        // Symbols alike in every state are one.
        let mut columns: HashMap<Vec<u32>, u32> = HashMap::new();
        let mut symbol_of = Vec::with_capacity(self.symbols);
        let mut kept = Vec::new();
        for s in 0..self.symbols {
            let column: Vec<u32> = representative
                .iter()
                .map(|&q| {
                    order[class[self.next[q * self.symbols + s] as usize] as usize].unwrap_or(0)
                })
                .collect();
            let len = columns.len() as u32;
            let id = *columns.entry(column).or_insert_with(|| {
                kept.push(s);
                len
            });
            symbol_of.push(id);
        }
        let symbols = kept.len();
        let mut sizes = vec![0u64; symbols];
        for s in 0..self.symbols {
            sizes[symbol_of[s] as usize] += self.sizes[s];
        }
        let mut ranges = Vec::with_capacity(self.ranges.len());
        for &(lo, hi, s) in &self.ranges {
            push_range(&mut ranges, lo, hi, symbol_of[s as usize]);
        }
        let mut next = Vec::with_capacity(states * symbols);
        for &q in &representative {
            for &s in &kept {
                next.push(
                    order[class[self.next[q * self.symbols + s] as usize] as usize].unwrap_or(0),
                );
            }
        }
        let accept = representative.iter().map(|&q| self.accept[q]).collect();
        Self {
            ranges,
            sizes,
            symbols,
            next,
            accept,
            profile: OnceLock::new(),
        }
    }

    fn profile(&self) -> &Profile {
        self.profile.get_or_init(|| {
            let n = self.states();
            let succ = |q: usize| self.next[q * self.symbols..(q + 1) * self.symbols].iter();
            let mut reachable = vec![false; n];
            let mut stack = vec![0usize];
            reachable[0] = true;
            while let Some(q) = stack.pop() {
                for &t in succ(q) {
                    if !reachable[t as usize] {
                        reachable[t as usize] = true;
                        stack.push(t as usize);
                    }
                }
            }
            let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n];
            for q in 0..n {
                for &t in succ(q) {
                    pred[t as usize].push(q);
                }
            }
            let mut live = self.accept.clone();
            let mut stack: Vec<usize> = (0..n).filter(|&q| live[q]).collect();
            while let Some(q) = stack.pop() {
                for &p in &pred[q] {
                    if !live[p] {
                        live[p] = true;
                        stack.push(p);
                    }
                }
            }
            let useful: Vec<bool> = (0..n).map(|q| reachable[q] && live[q]).collect();
            // A cycle among useful states: depth-first search for a back edge.
            let mut colour = vec![0u8; n];
            let mut cyclic = false;
            for root in (0..n).filter(|&q| useful[q]) {
                if colour[root] != 0 || cyclic {
                    continue;
                }
                let mut stack = vec![(root, 0usize)];
                colour[root] = 1;
                while let Some(&mut (q, ref mut s)) = stack.last_mut() {
                    if *s == self.symbols {
                        colour[q] = 2;
                        stack.pop();
                        continue;
                    }
                    let t = self.next[q * self.symbols + *s] as usize;
                    *s += 1;
                    if !useful[t] {
                        continue;
                    }
                    match colour[t] {
                        0 => {
                            colour[t] = 1;
                            stack.push((t, 0));
                        }
                        1 => {
                            cyclic = true;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Profile {
                useful_count: useful.iter().filter(|&&u| u).count(),
                useful,
                cyclic,
            }
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.profile().useful_count == 0
    }

    /// One step of the count: strings per state, one character longer.
    fn step(&self, v: &[u64], useful: &[bool]) -> Vec<u64> {
        let mut out = vec![0u64; v.len()];
        for (q, &n) in v.iter().enumerate() {
            if n == 0 {
                continue;
            }
            for s in 0..self.symbols {
                let t = self.next[q * self.symbols + s] as usize;
                if useful[t] {
                    out[t] = out[t].saturating_add(n.saturating_mul(self.sizes[s]));
                }
            }
        }
        out
    }

    fn accepted(&self, v: &[u64]) -> u64 {
        v.iter()
            .zip(&self.accept)
            .filter(|(_, a)| **a)
            .fold(0u64, |t, (n, _)| t.saturating_add(*n))
    }

    /// The states reached by some string of `length` characters, or `None` if finding them
    /// takes too long.
    fn reached_at(&self, length: i128, useful: &[bool]) -> Option<Vec<bool>> {
        let mut seen: HashMap<Vec<bool>, i128> = HashMap::new();
        let mut set = vec![false; self.states()];
        set[0] = useful[0];
        let mut n = 0i128;
        while n < length {
            if let Some(&first) = seen.get(&set) {
                // Periodic from `first` with period `n - first`.
                let period = n - first;
                let remaining = (length - n) % period;
                for _ in 0..remaining {
                    set = self.step_set(&set, useful);
                }
                return Some(set);
            }
            if seen.len() >= MAX_STATES {
                return None;
            }
            seen.insert(set.clone(), n);
            set = self.step_set(&set, useful);
            n += 1;
        }
        Some(set)
    }

    fn step_set(&self, set: &[bool], useful: &[bool]) -> Vec<bool> {
        let mut out = vec![false; set.len()];
        for q in (0..set.len()).filter(|&q| set[q]) {
            for s in 0..self.symbols {
                let t = self.next[q * self.symbols + s] as usize;
                out[t] |= useful[t];
            }
        }
        out
    }

    /// How many strings it accepts with a length in `lengths`.
    pub(crate) fn count(&self, lengths: &Line<i128>) -> Count {
        let profile = self.profile();
        if profile.useful_count == 0 {
            return Count::ZERO;
        }
        let useful = &profile.useful;
        let mut total = Count::ZERO;
        for segment in lengths.segments() {
            let (lo, hi) = integer_range(&segment);
            let lo = lo.unwrap_or(0).max(0);
            if hi.is_some_and(|hi| hi < lo) {
                continue;
            }
            if !profile.cyclic {
                // No string is longer than the useful states.
                let last = hi.map_or(profile.useful_count as i128, |hi| {
                    hi.min(profile.useful_count as i128)
                });
                let mut v = initial(self.states(), useful);
                for n in 0..=last {
                    if n >= lo {
                        total = total.plus(Count::exact(self.accepted(&v)));
                    }
                    v = self.step(&v, useful);
                }
                continue;
            }
            let Some(hi) = hi else {
                // Infinitely many lengths have strings.
                return Count::MANY;
            };
            let (mut v, exact) = if lo <= EXACT_UPTO {
                let mut v = initial(self.states(), useful);
                for _ in 0..lo {
                    v = self.step(&v, useful);
                }
                (v, true)
            } else {
                let Some(set) = self.reached_at(lo, useful) else {
                    total.hi = u64::MAX;
                    continue;
                };
                (set.into_iter().map(u64::from).collect(), false)
            };
            let last = hi.min(lo.saturating_add(WINDOW + profile.useful_count as i128));
            let mut part = 0u64;
            for n in lo..=last {
                part = part.saturating_add(self.accepted(&v));
                if n < last {
                    v = self.step(&v, useful);
                }
            }
            total = total.plus(if exact && last == hi {
                Count::exact(part)
            } else {
                Count {
                    lo: part,
                    hi: u64::MAX,
                }
            });
        }
        total
    }

    /// The strings it accepts with a length in `lengths`, in order of length then code
    /// points, if they are at most `limit`.
    pub(crate) fn strings(&self, lengths: &Line<i128>, limit: u64) -> Option<Vec<String>> {
        if self.count(lengths).hi > limit {
            return None;
        }
        let profile = self.profile();
        if profile.useful_count == 0 {
            return Some(Vec::new());
        }
        let mut out = Vec::new();
        for segment in lengths.segments() {
            let (lo, hi) = integer_range(&segment);
            let lo = lo.unwrap_or(0).max(0);
            // Finite here: the count was.
            let hi = hi.unwrap_or(profile.useful_count as i128);
            for n in lo..=hi {
                let n = usize::try_from(n).ok()?;
                let alive = self.alive(n, &profile.useful);
                let mut word = Vec::with_capacity(n);
                self.spell(0, n, &alive, &mut word, &mut out, limit)?;
            }
        }
        Some(out)
    }

    /// `alive[k][q]`: some string of `k` characters leads from `q` to an acceptance.
    fn alive(&self, n: usize, useful: &[bool]) -> Vec<Vec<bool>> {
        let mut alive = vec![
            self.accept
                .iter()
                .zip(useful)
                .map(|(a, u)| *a && *u)
                .collect::<Vec<_>>(),
        ];
        for k in 1..=n {
            let before = &alive[k - 1];
            let row = (0..self.states())
                .map(|q| {
                    (0..self.symbols).any(|s| before[self.next[q * self.symbols + s] as usize])
                })
                .collect();
            alive.push(row);
        }
        alive
    }

    fn spell(
        &self,
        q: usize,
        remaining: usize,
        alive: &[Vec<bool>],
        word: &mut Vec<char>,
        out: &mut Vec<String>,
        limit: u64,
    ) -> Option<()> {
        if !alive[remaining][q] {
            return Some(());
        }
        if remaining == 0 {
            out.push(word.iter().collect());
            return (out.len() as u64 <= limit).then_some(());
        }
        for &(lo, hi, s) in &self.ranges {
            let t = self.next[q * self.symbols + s as usize] as usize;
            if !alive[remaining - 1][t] {
                continue;
            }
            for c in (lo..=hi).filter_map(char::from_u32) {
                word.push(c);
                self.spell(t, remaining - 1, alive, word, out, limit)?;
                word.pop();
            }
        }
        Some(())
    }
}

fn initial(states: usize, useful: &[bool]) -> Vec<u64> {
    let mut v = vec![0u64; states];
    v[0] = u64::from(useful[0]);
    v
}

fn push_range(ranges: &mut Vec<(u32, u32, u32)>, lo: u32, hi: u32, symbol: u32) {
    match ranges.last_mut() {
        Some(last) if last.2 == symbol && last.1.checked_add(1) == Some(lo) => last.1 = hi,
        _ => ranges.push((lo, hi, symbol)),
    }
}

/// Thompson's automaton: ε-moves and moves on a class of characters.
#[derive(Default)]
struct Nfa {
    eps: Vec<Vec<u32>>,
    moves: Vec<Vec<(u32, u32)>>,
    classes: Vec<CharSet>,
    class_ids: HashMap<CharSet, u32>,
}

impl Nfa {
    fn state(&mut self) -> Result<u32, PatternError> {
        if self.eps.len() >= MAX_STATES {
            return Err(PatternError {
                at: 0,
                message: format!("the pattern needs more than {MAX_STATES} states"),
            });
        }
        self.eps.push(Vec::new());
        self.moves.push(Vec::new());
        Ok((self.eps.len() - 1) as u32)
    }

    fn build(&mut self, ast: &Ast) -> Result<(u32, u32), PatternError> {
        let start = self.state()?;
        let end = self.state()?;
        match ast {
            Ast::Class(set) => {
                let len = self.class_ids.len() as u32;
                let class = *self.class_ids.entry(set.clone()).or_insert_with(|| {
                    self.classes.push(set.clone());
                    len
                });
                self.moves[start as usize].push((class, end));
            }
            Ast::Concat(parts) => {
                let mut at = start;
                for part in parts {
                    let (s, e) = self.build(part)?;
                    self.eps[at as usize].push(s);
                    at = e;
                }
                self.eps[at as usize].push(end);
            }
            Ast::Alt(branches) => {
                for branch in branches {
                    let (s, e) = self.build(branch)?;
                    self.eps[start as usize].push(s);
                    self.eps[e as usize].push(end);
                }
            }
            Ast::Repeat(inner, min, max) => {
                let mut at = start;
                for _ in 0..*min {
                    let (s, e) = self.build(inner)?;
                    self.eps[at as usize].push(s);
                    at = e;
                }
                match max {
                    None => {
                        let (s, e) = self.build(inner)?;
                        self.eps[at as usize].push(s);
                        self.eps[e as usize].push(s);
                        self.eps[e as usize].push(end);
                        self.eps[at as usize].push(end);
                    }
                    Some(max) => {
                        for _ in *min..*max {
                            let (s, e) = self.build(inner)?;
                            self.eps[at as usize].push(s);
                            self.eps[at as usize].push(end);
                            at = e;
                        }
                        self.eps[at as usize].push(end);
                    }
                }
            }
        }
        Ok((start, end))
    }

    fn closure(&self, seeds: &[u32]) -> Vec<u32> {
        let mut seen = vec![false; self.eps.len()];
        let mut stack: Vec<u32> = seeds.to_vec();
        let mut out = Vec::new();
        while let Some(q) = stack.pop() {
            if std::mem::replace(&mut seen[q as usize], true) {
                continue;
            }
            out.push(q);
            stack.extend(&self.eps[q as usize]);
        }
        out.sort_unstable();
        out
    }

    /// The subset construction, over the partition of `Char` the classes induce.
    fn determinise(&self, start: u32, end: u32) -> Result<Dfa, PatternError> {
        let (ranges, sizes, class_symbols) = partition(&self.classes);
        let symbols = sizes.len();
        let mut ids: HashMap<Vec<u32>, u32> = HashMap::new();
        let first = self.closure(&[start]);
        ids.insert(first.clone(), 0);
        let mut sets = vec![first];
        let mut next = Vec::new();
        let mut accept = Vec::new();
        let mut k = 0;
        while k < sets.len() {
            if sets.len() > MAX_STATES {
                return Err(PatternError {
                    at: 0,
                    message: format!("the pattern needs more than {MAX_STATES} states"),
                });
            }
            let set = sets[k].clone();
            k += 1;
            accept.push(set.binary_search(&end).is_ok());
            let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); symbols];
            for &q in &set {
                for &(class, t) in &self.moves[q as usize] {
                    for &s in &class_symbols[class as usize] {
                        buckets[s as usize].push(t);
                    }
                }
            }
            let mut closed: HashMap<Vec<u32>, u32> = HashMap::new();
            for mut bucket in buckets {
                bucket.sort_unstable();
                bucket.dedup();
                let id = match closed.get(&bucket) {
                    Some(&id) => id,
                    None => {
                        let target = self.closure(&bucket);
                        let len = ids.len() as u32;
                        let id = *ids.entry(target.clone()).or_insert_with(|| {
                            sets.push(target);
                            len
                        });
                        closed.insert(bucket, id);
                        id
                    }
                };
                next.push(id);
            }
        }
        Ok(Dfa {
            ranges,
            sizes,
            symbols,
            next,
            accept,
            profile: OnceLock::new(),
        })
    }
}

/// The coarsest partition of `Char` into symbols no class splits: `(ranges, sizes, the
/// symbols of each class)`.
#[allow(clippy::type_complexity)]
fn partition(classes: &[CharSet]) -> (Vec<(u32, u32, u32)>, Vec<u64>, Vec<Vec<u32>>) {
    let all = CharSet::all();
    let mut cuts: Vec<u32> = all
        .ranges()
        .iter()
        .chain(classes.iter().flat_map(CharSet::ranges))
        .flat_map(|&(lo, hi)| [lo, hi + 1])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut signatures: HashMap<Vec<bool>, u32> = HashMap::new();
    let mut sizes: Vec<u64> = Vec::new();
    let mut ranges = Vec::new();
    let mut class_symbols: Vec<Vec<u32>> = vec![Vec::new(); classes.len()];
    for w in cuts.windows(2) {
        let (lo, hi) = (w[0], w[1] - 1);
        let Some(c) = char::from_u32(lo) else {
            continue;
        };
        if !all.contains(c) {
            continue;
        }
        let signature: Vec<bool> = classes.iter().map(|k| k.contains(c)).collect();
        let len = signatures.len() as u32;
        let symbol = *signatures.entry(signature.clone()).or_insert_with(|| {
            sizes.push(0);
            for (k, &member) in signature.iter().enumerate() {
                if member {
                    class_symbols[k].push(len);
                }
            }
            len
        });
        sizes[symbol as usize] += u64::from(hi - lo) + 1;
        push_range(&mut ranges, lo, hi, symbol);
    }
    (ranges, sizes, class_symbols)
}
