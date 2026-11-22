//! Old consistency instances keyed by schema bindings or by the members of a list pair.

use nrese_exec::{Budget, ChargedMap, ChargedVec, Reservation, memory::MemoryStopCause};

use crate::ir::{Rule, Triple};

use super::{
    GroundProgram, Grounded, OperationStop, Poller, Source, Stopped, bind, guards_hold, pattern,
};

type Substitution<'p> = &'p [Option<u64>];
type Projections<'p, 'm> = ChargedVec<Substitution<'p>, &'m Budget>;
const NO_INSTANCE: usize = usize::MAX;

#[derive(Clone, Copy)]
enum PairRule {
    Properties,
    Classes,
    Individuals,
}

impl PairRule {
    fn of(rule: &Rule) -> Option<Self> {
        match (rule.name.as_str(), rule.body.len()) {
            ("prp-adp", 2) => Some(Self::Properties),
            ("cax-adc", 2) => Some(Self::Classes),
            ("eq-diff2" | "eq-diff3", 1) => Some(Self::Individuals),
            _ => None,
        }
    }

    fn premises(rule: &Rule, bindings: &[Option<u64>]) -> [[Option<u64>; 3]; 2] {
        let first = pattern(&rule.body[0], bindings);
        [
            first,
            rule.body
                .get(1)
                .map_or(first, |atom| pattern(atom, bindings)),
        ]
    }

    fn members<T: Copy + Ord>(self, [left, right]: [[T; 3]; 2]) -> [T; 2] {
        let mut members = match self {
            Self::Properties => [left[1], right[1]],
            Self::Classes => [left[2], right[2]],
            Self::Individuals => [left[0], left[2]],
        };
        members.sort_unstable();
        members
    }

    fn holds_before<S: Source + ?Sized>(
        self,
        source: &S,
        rule: &Rule,
        grounded: &Grounded,
        [left, right]: [Triple; 2],
        poll: &Poller<'_>,
    ) -> Result<bool, Stopped> {
        let [s, p, o] = left;
        // The indexed pair orders distinct members. The enumerated rule can
        // have them in either order, including the two ends of an equality.
        let swapped = match self {
            Self::Individuals => [[o, p, s]; 2],
            _ => [right, left],
        };
        let mut scratch = [None; u8::MAX as usize + 1];
        let bindings = &mut scratch[..grounded.substitution.len()];
        for premises in [[left, right], swapped] {
            bindings.copy_from_slice(&grounded.substitution);
            if rule
                .body
                .iter()
                .zip(premises)
                .all(|(atom, fact)| bind(atom, fact, bindings).is_some())
                && guards_hold(&grounded.rule.guards, bindings, poll)
                && source.contains(premises[0], poll)?
                && (premises[0] == premises[1] || source.contains(premises[1], poll)?)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

type PairPattern<'p> = (&'p str, [Option<u64>; 2]);

/// The distinct schema projections per rule name and witness width, plus the old instances
/// under each full key. Several instances can share a key and differ in bodies or guards.
pub(crate) struct WitnessIndex<'p, 'm> {
    program: &'p GroundProgram,
    projections: ChargedMap<(&'p str, usize), Projections<'p, 'm>, Reservation<&'m Budget>>,
    heads: ChargedMap<(&'p str, Substitution<'p>), usize, Reservation<&'m Budget>>,
    pairs: ChargedMap<PairPattern<'p>, usize, Reservation<&'m Budget>>,
    next: ChargedVec<usize, &'m Budget>,
}

impl<'p, 'm> WitnessIndex<'p, 'm> {
    pub(crate) fn new(
        program: &'p GroundProgram,
        memory: &'m Budget,
        operation: &OperationStop<'_>,
        poll: &Poller<'_>,
    ) -> Result<Self, Stopped> {
        let refused = |_| {
            operation.refuse(MemoryStopCause::OverLimit);
            Stopped
        };
        let mut index = Self {
            program,
            projections: ChargedMap::new(memory),
            heads: ChargedMap::new(memory),
            pairs: ChargedMap::new(memory),
            next: ChargedVec::with_capacity(memory, program.consistency.len()).map_err(refused)?,
        };
        for (c, (rule, grounded)) in program.consistency.iter().enumerate() {
            if poll.tick() {
                return Err(Stopped);
            }
            #[cfg(test)]
            crate::witness::add(crate::witness::Site::OldWitnessInstance, 1);
            let name = rule.name.as_str();
            let substitution = grounded.substitution.as_slice();
            if let Some(pair) = PairRule::of(rule) {
                let members = pair.members(PairRule::premises(rule, substitution));
                let (head, _) = index
                    .pairs
                    .get_or_insert_with((name, members), || NO_INSTANCE)
                    .map_err(refused)?;
                index
                    .next
                    .push(std::mem::replace(head, c))
                    .map_err(refused)?;
                continue;
            }
            let (head, new_key) = index
                .heads
                .get_or_insert_with((name, substitution), || NO_INSTANCE)
                .map_err(refused)?;
            index
                .next
                .push(std::mem::replace(head, c))
                .map_err(refused)?;
            if new_key {
                let (layouts, _) = index
                    .projections
                    .get_or_insert_with((name, substitution.len()), || ChargedVec::new(memory))
                    .map_err(refused)?;
                let known = poll.counted(layouts.iter()).any(|layout| {
                    layout
                        .iter()
                        .zip(substitution)
                        .all(|(a, b)| a.is_some() == b.is_some())
                });
                poll.ended()?;
                if !known {
                    layouts.push(substitution).map_err(refused)?;
                }
            }
        }
        Ok(index)
    }

    /// The ground schema was already checked in the before-program. Probe the matching
    /// instances' remaining body atoms and guards in the before state.
    pub(crate) fn contains<S: Source + ?Sized>(
        &self,
        source: &S,
        rule: &Rule,
        bindings: &[Option<u64>],
        poll: &Poller<'_>,
    ) -> Result<bool, Stopped> {
        let name = rule.name.as_str();
        if let Some(pair) = PairRule::of(rule) {
            let [left, right] = PairRule::premises(rule, bindings);
            let ([Some(s), Some(p), Some(o)], [Some(t), Some(q), Some(u)]) = (left, right) else {
                return Ok(false);
            };
            return self.contains_pair(source, name, pair, [[s, p, o], [t, q, u]], poll);
        }
        let Some(layouts) = self.projections.get(&(name, bindings.len())) else {
            return Ok(false);
        };
        let mut scratch = [None; u8::MAX as usize + 1];
        let projected = &mut scratch[..bindings.len()];
        for layout in poll.counted(layouts.iter()) {
            for ((slot, value), bound) in projected.iter_mut().zip(bindings).zip(*layout) {
                *slot = if bound.is_some() { *value } else { None };
            }
            let mut candidate = self
                .heads
                .get(&(name, &*projected))
                .copied()
                .unwrap_or(NO_INSTANCE);
            'instances: while candidate != NO_INSTANCE {
                if poll.tick() {
                    return Err(Stopped);
                }
                #[cfg(test)]
                crate::witness::add(crate::witness::Site::OldWitnessInstance, 1);
                let grounded = &self.program.consistency[candidate].1;
                candidate = self.next[candidate];
                for atom in &grounded.rule.body {
                    let [Some(s), Some(p), Some(o)] = pattern(atom, bindings) else {
                        continue 'instances;
                    };
                    if !source.contains([s, p, o], poll)? {
                        continue 'instances;
                    }
                }
                if guards_hold(&grounded.rule.guards, bindings, poll) {
                    return Ok(true);
                }
            }
        }
        poll.ended()?;
        Ok(false)
    }

    fn contains_pair<S: Source + ?Sized>(
        &self,
        source: &S,
        kind: &str,
        pair: PairRule,
        premises: [Triple; 2],
        poll: &Poller<'_>,
    ) -> Result<bool, Stopped> {
        let [a, b] = pair.members(premises).map(Some);
        for members in [[a, b], [None, a], [None, b], [None, None]] {
            let mut candidate = self
                .pairs
                .get(&(kind, members))
                .copied()
                .unwrap_or(NO_INSTANCE);
            while candidate != NO_INSTANCE {
                if poll.tick() {
                    return Err(Stopped);
                }
                #[cfg(test)]
                crate::witness::add(crate::witness::Site::OldWitnessInstance, 1);
                let (rule, grounded) = &self.program.consistency[candidate];
                if pair.holds_before(source, rule, grounded, premises, poll)? {
                    return Ok(true);
                }
                candidate = self.next[candidate];
            }
        }
        poll.ended()?;
        Ok(false)
    }
}
